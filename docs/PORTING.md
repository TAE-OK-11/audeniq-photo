# 외부 의존성 포팅 계획과 1차 결과

## 1. Audeniq 백엔드가 직접 쓰던 이미지 관련 외부 도구

| 도구 | 백엔드 위치 | 용도 | 호출 빈도 |
|---|---|---|---|
| `python3` + Pillow + LittleCMS (`deploy/sanitize-upload.py`) | `upload_safety::sanitize` | JPEG/PNG 재인코딩(EXIF 회전, ICC→sRGB, 메타데이터 제거), 전자서명 PNG 검증, PDF 이미지화 | 모든 이미지·서명·문서 업로드 |
| ExifTool (Perl) | `provenance::inspect`, `artwork_policy::color` | AI 생성 메타데이터 신호, 커버 색상 속성 | 모든 이미지·오디오 업로드 + 커버 QC 2회 |
| `zbarimg` | `artwork_policy::qr` | 커버 QR 개수 | 커버 QC마다 |
| `ffprobe` (이미지) | `qc::check_image` | 커버 크기·디코딩 가능 여부 | 커버 QC마다 |
| `ffprobe` (오디오 태그) | `provenance::inspect_audio` 대체 경로 | ExifTool이 못 읽는 TTA 태그 | TTA 업로드 |
| Poppler `pdfinfo`/`pdftoppm` | 정화기 PDF 경로 | 문서 래스터화 | 문서 업로드 |
| Tesseract (eng+kor) | `artwork_policy::text` | 커버 OCR | 커버 QC마다 |

이미지와 무관한 외부 의존성(ffmpeg 오디오 QC·지문·FLAC 변환, xmllint, clamd, sftp)은 이 저장소 범위 밖입니다.

## 2. 대체 가치 순위

호출 빈도 × 1회 비용(프로세스 생성·인터프리터 기동·메모리) × 보안 표면 × 포팅 난이도 기준.

| 순위 | 대상 | 근거 | 1차 |
|---|---|---|---|
| 1 | Python/Pillow/LittleCMS 정화기 | 업로드마다 인터프리터+Pillow 기동(≈60 ms, 45–120 MB), 신뢰할 수 없는 바이트를 C 디코더가 처리 | **완료** |
| 2 | ExifTool | 업로드·QC마다 Perl 기동(≈80 ms, 18–45 MB), 커버당 2회 | **완료** |
| 3 | zbarimg | 커버당 0.1–0.6 s, 90 MB | **완료** |
| 4 | ffprobe(이미지) | 헤더만 읽는데 프로세스 40–215 ms, 50–94 MB | **완료** |
| 5 | ffprobe(오디오 태그 대체 경로) | 네이티브 리더가 TTA를 직접 읽으면 불필요 | **완료** |
| 6 | Poppler | 문서 업로드만(드묾), PDF 파서+래스터라이저는 대규모 | **2차 완료** (hayro를 가져와 합침) |
| 7 | Tesseract | 커버마다 실행되지만 LSTM 엔진·eng/kor 모델 포팅은 별도 대형 과제 | **완료** (3차) |

## 3. 백엔드 전환 내용 (1차)

- `qc::check_image`: ffprobe → `audeniq_photo::probe` (헤더 파싱, 프로세스 없음)
- `artwork_policy::color`: ExifTool → `audeniq_photo::color_report` (같은 JSON 형태)
- `artwork_policy::qr`: zbarimg → `audeniq_photo::qr_count` (JPEG는 휘도 평면만 디코드)
- `provenance::inspect` / `inspect_audio`: ExifTool + ffprobe 대체 경로 → `audeniq_photo::metadata_file` (오디오 샘플 데이터는 읽지 않음)
- `upload_safety::sanitize`: Python 정화기 → `audeniq_photo::sanitize`; PDF는 Poppler를 기존 샌드박스로 실행하고 이미지 전용 PDF 작성은 Rust
- 런타임 이미지에서 `libimage-exiftool-perl`, `zbar-tools`, `python3-pil` 제거 (python3는 운영 스크립트용으로 유지)

보안 모델 변화: 정화·메타데이터·QR 디코딩이 샌드박스 자식 프로세스 대신 워커 프로세스 안에서 실행됩니다.
대신 모든 디코더가 메모리 안전한 Rust(`unsafe` 금지)이고, 픽셀 수·할당 크기·텍스트 크기 한도와
데드라인, 패닉 격리(`catch_unwind`)를 갖습니다. PDF는 렌더링 중 중단이 불가능하므로 둘로 나눕니다:
파싱·렌더링(`pdf::rasterize_frames`)은 기존 Landlock/seccomp 샌드박스(시간·메모리 한도) 자식 프로세스에서(Poppler 자리)
원시 RGB 프레임만 내보내고, 부모가 프레임을 검증(쪽수·크기·픽셀 예산·정확한 길이)해 이미지 전용 PDF를 직접 씁니다
(`pdf::image_only_pdf_from_frames`). 자식이 오염돼도 결과물에는 픽셀만 들어갑니다. OCR은 Tesseract 대신 `audeniq_photo::ocr_tsv`가 프로세스 안에서 수행합니다.

## 4. 원칙: Rust 구현은 가져와 개선, 나머지는 포팅

- 성숙한 Rust 구현이 있는 영역은 벤더링해 Audeniq 요구에 맞게 조정합니다.
  - **zlib**: 자체 deflate 대신 [zlib-rs](https://github.com/trifectatechfoundation/zlib-rs)(zlib-ng 포트)의 엔진을 `photo-deflate`
    내부 모듈로 합쳤습니다(`crates/photo-deflate/ENGINE.md`). 별도 크레이트·래퍼 없이 우리 API가 엔진을 직접 호출해 호출자 버퍼에 바로
    쓰고(0 채우기·임시 버퍼 복사 제거), 스레드별로 스트림 상태를 재사용하며, CPU 기능은 프로세스당 한 번 판별합니다. C 할당자·콜백 API·
    LoongArch/wasm 경로는 제거했고 알고리즘·SIMD 커널·업스트림 단위 테스트는 유지합니다.
    결과: 3000px PNG 정화 1.11 s → 0.73 s, inflate 159 → 110 ms, PNG 디코드 130 → 84 ms (런타임 AVX2/PCLMUL 선택이라 기본 빌드에서도 적용).
- **PDF**: 성숙한 순수 Rust 렌더러 [hayro](https://github.com/LaurenzV/hayro)를 `photo-pdf*` 크레이트로 합쳤습니다
  (`crates/photo-pdf/UPSTREAM.md`). Flate는 `photo-deflate`, JPEG는 `photo-jpeg`로 바꿔 같은 엔진을 공유하고,
  압축 폭탄·픽셀 한도를 넣었으며, 메시 셰이딩 샘플링을 해시맵에서 잘린 조밀 격자로 바꿔 최악 메모리를 1978 MB → 96 MB로 줄였습니다.
  `pdf::sanitize_pdf`가 `pdfinfo` → `pdftoppm` → JPEG 파일 → 재디코드 → 재인코드 과정을 한 번의 렌더·인코드로 대신합니다
  (중간 JPEG 손실 단계가 사라짐). 페이지 상자는 뷰어가 보여주는 CropBox를 그립니다(Poppler 기본값은 MediaBox).
- C/C++/Perl/Python 도구(libjpeg-turbo, LittleCMS, ExifTool, ZBar/quirc, Pillow 정화기, 다음으로 Poppler·Tesseract)는 Rust로 포팅합니다.

## 5. 장기 계획

1. **2차 (완료)**: PDF — hayro를 가져와 합치고 Poppler 제거. 남은 합칠 대상: `moxcms` → `photo-icc`, `pic-scale` → 자체 리샘플러,
   이후 `vello_cpu`·`skrifa`·`kurbo`
2. **3차 (완료)**: OCR — `photo-ocr`.
   - 1단계 완료: traineddata 로더, int8 LSTM 망(Convolve·Maxpool·LSTM·요약 LSTM·역방향·전치·Softmax), `TRand`(minstd)까지
     같은 난수, Leptonica 전처리(`pixScale`의 LI·면적 평균·2x/4x·unsharp mask, 컬러는 채널별 축소 후 휘도, 반전 재시도),
     unicharset·재부호기(한글 자모 코드), DAWG 사전, Tesseract 힙을 그대로 옮긴 CTC 빔 탐색, 단어 분리·신뢰도.
     `tesseract --psm 13`과 단어·신뢰도가 소수 6자리까지 같음(빈 줄의 환각 출력까지 동일). 한 줄 약 22 ms(SIMD 전).
   - 2단계 완료: `--psm 11`(희소 텍스트) 페이지 배치 분석 — Otsu 이진화, 선 제거, 사진 영역, 연결 성분·윤곽선,
     획 폭·탭 찾기·열 분할(ColumnFinder 희소 경로), 기준선(x87 80비트 확장 정밀도까지 재현한 QLSQ), 행·x높이,
     밑줄 분리, 고정 피치·간격 통계·단어 분할, 잡음 정리, 발음 구별 부호 전달. 단계마다 Tesseract 계측 덤프와 52개 표지 일치.
   - 3단계 완료: 단어 단위 LSTM 인식(행 기준선으로 자른 원본 이미지), `eng+kor` 언어 재시도(`SelectBestWords`),
     원래 블롭 재분배로 단어 상자, TSV. 생성 커버 172개(PNG·JPEG)에서 `tesseract -l eng+kor --psm 11 tsv`와 바이트 단위 동일.
     워드 단위 rasterop과 런타임 AVX2 int8 커널로 단일 스레드 Tesseract보다 1.5배 빠름(52개 표지 15.7초 대 23.9초).
     백엔드 `artwork_policy::text`가 `audeniq_photo::ocr_tsv`를 쓰고 `tesseract-ocr` 패키지를 제거.
3. 자체 개발 단계 (진행 중): 포팅 코드를 기준선으로 고정(비트 동일 테스트)한 뒤 최적화.
   - 완료: `photo_core::multiversion!` — 뜨거운 루프를 x86-64-v3용으로 한 번 더 컴파일해 실행 중 선택
     (기본 빌드는 이식성 유지, `AUDENIQ_PHOTO_NO_SIMD`/`AUDENIQ_PHOTO_NO_VNNI`로 끄기).
   - 완료: JPEG — FDCT/IDCT 8레인화, 역수 양자화 SoA, DC 전용 블록 지름길 (인코딩 220→80 ms, 디코딩 93→77 ms).
   - 완료: PNG — 자체 매치파인더 `Strategy::Image`(8바이트 해시 1칸 표, 블록 통계 기반 비용 비교로 긴 일치만,
     연속 실패 시 탐색 간격 확대), 다섯 필터 합 한 번에 계산, 표본 대역으로 이미지/zlib 튜닝 선택.
   - 완료: QR — 임계값 이동 평균을 4단계 선형 점화식(f64)으로, 64픽셀 비트마스크 런 추출.
   - 완료: OCR — int8 가중치 8행 블록 재배치 + `vpmaddwd`/AVX-VNNI `vpdpbusd`(입력 +128 오프셋을 행 상수로 보정),
     tanh/logistic 표 보간 AVX2 gather, 상자 화소 수 popcount. 52개 표지 14.0→7.4 s.
   - 남음: 메타데이터 C2PA(JUMBF) 판독, JPEG 디코더 MCU 행 단위 스트리밍(메모리 절반), PDF 렌더러 최적화
4. 오디오 도구(ffmpeg/ffprobe) 포팅은 별도 저장소에서 같은 원칙으로 진행하고, 공통 크레이트(`photo-core`, `photo-deflate`)를 공유

## 벤치마크

`audeniq-photo-bench --iterations 5 --threads 4` (4 vCPU Intel Xeon 2.1 GHz, AVX-VNNI). 외부 측정은 자식 프로세스의
`ru_maxrss`/CPU, Rust는 프로세스 전체 VmHWM(측정마다 초기화)이라 Rust 쪽 RSS에는 벤치 프로세스 자체와 입력 버퍼가 포함됩니다.

### 기본 빌드 (x86-64, 런타임 AVX2/AVX-VNNI 선택; 2026-10 최적화 후)

| file | operation | Rust wall ms | Rust CPU ms | Rust peak RSS MB | external wall ms | external CPU ms | external peak RSS MB | speed-up |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| cover_3000.jpg | probe | 1 µs | 0 µs | 66.2 | 53.2 | 53.0 | 66.2 | 59180.5× |
| cover_3000.jpg | color | 1 µs | 0 µs | 66.2 | 99.2 | 99.0 | 66.2 | 69027.9× |
| cover_3000.jpg | provenance | 2 µs | 0 µs | 66.2 | 98.9 | 98.7 | 66.2 | 62961.6× |
| cover_3000.jpg | qr | 106.9 | 108.0 | 59.4 | 542.3 | 542.0 | 90.7 | 5.1× |
| cover_3000.jpg | cover (all of the above) | 115.1 | 112.0 | 67.5 | 798.6 | 797.6 | 90.8 | 6.9× |
| cover_3000.jpg | sanitize | 156.1 | 156.0 | 72.6 | 279.3 | 276.8 | 120.2 | 1.8× |
| cover_3000.png | probe | 729 µs | 0 µs | 28.2 | 249.0 | 248.6 | 94.2 | 341.6× |
| cover_3000.png | color | 3 µs | 0 µs | 28.2 | 106.6 | 106.3 | 28.0 | 37023.0× |
| cover_3000.png | provenance | 3 µs | 0 µs | 28.2 | 99.0 | 98.9 | 28.2 | 34689.1× |
| cover_3000.png | qr | 173.6 | 176.0 | 80.5 | 668.9 | 668.6 | 91.1 | 3.9× |
| cover_3000.png | cover (all of the above) | 182.5 | 183.7 | 107.0 | 1167.4 | 1166.2 | 107.0 | 6.4× |
| cover_3000.png | sanitize | 711.9 | 711.9 | 74.7 | 2801.0 | 2800.2 | 120.2 | 3.9× |
| cover_1400.jpg | probe | 1 µs | 0 µs | 80.9 | 48.1 | 48.0 | 80.8 | 71741.2× |
| cover_1400.jpg | color | 2 µs | 0 µs | 80.9 | 96.3 | 96.1 | 80.8 | 42185.5× |
| cover_1400.jpg | provenance | 1 µs | 0 µs | 80.9 | 92.6 | 92.4 | 80.8 | 64530.6× |
| cover_1400.jpg | qr | 18.1 | 20.0 | 44.2 | 114.8 | 114.6 | 44.0 | 6.3× |
| cover_1400.jpg | cover (all of the above) | 18.6 | 20.0 | 44.2 | 342.6 | 341.8 | 49.2 | 18.4× |
| cover_1400.jpg | sanitize | 31.2 | 32.0 | 39.6 | 116.4 | 115.5 | 39.8 | 3.7× |
| cover_1400_adobergb.jpg | probe | 1 µs | 0 µs | 39.6 | 58.8 | 58.6 | 49.3 | 42033.5× |
| cover_1400_adobergb.jpg | color | 8 µs | 0 µs | 39.6 | 99.2 | 99.0 | 29.2 | 12267.0× |
| cover_1400_adobergb.jpg | provenance | 4 µs | 0 µs | 29.3 | 105.6 | 105.5 | 29.4 | 28766.4× |
| cover_1400_adobergb.jpg | qr | 19.7 | 20.0 | 40.1 | 107.4 | 107.2 | 40.1 | 5.5× |
| cover_1400_adobergb.jpg | cover (all of the above) | 19.2 | 20.0 | 40.1 | 365.1 | 364.3 | 49.1 | 19.1× |
| cover_1400_adobergb.jpg | sanitize | 41.3 | 40.0 | 38.6 | 174.4 | 174.1 | 40.1 | 4.2× |
| signature.png | sanitize | 1.5 | 0 µs | 29.3 | 78.0 | 77.3 | 29.3 | 51.0× |
| document_scan_3p.pdf | pdf sanitize (vs pdfinfo+pdftoppm+rebuild) | 180.0 | 180.0 | 64.1 | 372.1 | 371.7 | 62.4 | 2.1× |
| document_text_2p.pdf | pdf sanitize (vs pdfinfo+pdftoppm+rebuild) | 87.3 | 88.0 | 69.0 | 171.8 | 168.7 | 75.7 | 2.0× |

Sanitize throughput with 4 threads: Rust 19.0 files/s, Python/Pillow 5.9 files/s (3.2×)

| file | sanitized size (Rust) | sanitized size (Python/Pillow) |
|---|---:|---:|
| cover_3000.jpg | 4353 KiB | 4353 KiB |
| cover_3000.png | 10981 KiB | 10529 KiB |
| cover_1400.jpg | 730 KiB | 730 KiB |
| cover_1400_adobergb.jpg | 866 KiB | 866 KiB |
| signature.png | 2 KiB | 2 KiB |

Rust peak RSS is the whole benchmark process (VmHWM, reset before each run); external figures are the child's ru_maxrss. CPU is user+system time.

이전 측정(최적화 전)과 비교: 3000px JPEG 정화 285 → 156 ms, 커버 종합 검사 3000px JPEG 163 → 115 ms,
PDF 3쪽 스캔 263 → 180 ms, 정화 처리량 17.6 → 19.0 files/s. 런타임 선택이 생겨 `-C target-cpu=x86-64-v3` 빌드는
더 이상 필요하지 않습니다.
