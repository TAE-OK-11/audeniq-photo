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
   - 완료: JPEG 디코더 MCU 행 단위 스트리밍 — 단일 스캔 순차 JPEG은 성분마다 MCU 행 3개 고리 버퍼에 IDCT하고 한 행
     늦게 업샘플·색 변환해 출력에 바로 씀. 3000px 최대 힙 4:2:0 40.6→27.2 MB, 4:4:4 54.0→27.2 MB, 휘도 전용(QR)
     18.0→9.1 MB(출력 크기만큼). 프로그레시브는 계수를 끝까지 보관해야 해서 그대로(40.6 MB). 171개 파일 비트 동일.
   - 완료: PDF — PostScript 계산기 함수(Type 4)를 레지스터 코드로 컴파일: 스택 연산은 컴파일 때 레지스터 이름
     바꾸기로, 상수 부분식은 미리 계산(인터프리터와 같은 연산 함수), `if`/`ifelse`는 점프로, 갈래마다 결과 개수가
     다르면 나머지 프로그램을 갈래별로 복사. 정적이지 않은 프로그램은 기존 인터프리터. 셰이딩 페이지 2.4~2.7배,
     hayro 시험 PDF 368개 래스터 결과 동일·전체 36.5→23.5 s. 렌더러 페이지 버퍼 두 개(RGB→RGBA, RGBA→RGB)를
     제자리 변환으로 제거.
   - 완료: PNG — 원본 2 MiB 조각이 둘 이상인 이미지는 행 조각을 병렬 압축(pigz 방식: 앞 32 KiB 필터 결과를
     사전으로, sync flush, Adler-32 합성). 조각은 이미지로만 정해져 스레드 수와 무관하게 같은 출력.
     인코딩만 3000px zlib 경로 636→208 ms, 사진형 224→78 ms(4코어). 3000px 정화 0.71→0.31 s(벽시계; 프로세스
     CPU는 약 9% 늘어 0.78 s), 화소 동일, 크기 −0.8~+0.05%.
   - 완료: 메모리·안정성 정리 (`photo-bench/examples/heap_peak`: mimalloc 위 계수 할당기로 작업별 최대 힙, 5회 중 최소 시간).
     프로그레시브 JPEG은 계수를 지그재그 순서로 보관(대역이 연속 구간)하고 정밀화 단계에서 0 아닌 계수를 비트마스크로
     찾아 보정 비트를 최대 32개씩 읽음, AC 첫 단계에 빠른 AC 표 사용(3000px 디코딩 195→168 ms, Pillow 198개 비트 동일).
     EXIF 방향 2·3·4는 제자리 뒤집기, 5~8은 타일 단위 한 번 복사(3000px 회전 JPEG 정화 194→169 ms).
     QR 입력 휘도는 디코딩 버퍼 안에서 계산(CMYK는 RGB 중간 버퍼 없이)하고 임계값 처리 뒤 바로 해제
     (CMYK JPEG 커버 검사 72→45 MB). PNG는 IDAT 조각을 이어 붙이지 않고 차례로 inflate(압축 크기만큼 덜 씀),
     병렬 압축은 호출 스레드도 일하고 스레드 생성 실패 시 남은 스레드로 계속(오류 대신), 조각은 출력에 쓴 즉시 해제.
     정화의 알파 제거·흑백→RGB 확장은 제자리.
   - 완료: PDF 정화 — 호출 스레드가 다음 쪽을 렌더링하는 동안(렌더러 캐시는 스레드 안전하지 않아 한 스레드에 둠)
     보조 스레드가 이전 쪽을 JPEG 인코딩·기록. 3쪽 스캔 212→166 ms, 2쪽 텍스트 79→57 ms, 출력 바이트 동일,
     최대 힙은 래스터 한 장만큼(+11~13 MB) 늘어남. 프레임 조립(`pdf-assemble`)은 읽기가 가벼워 순차 그대로.
     보조 스레드를 만들 수 없으면 순차로 처리.
   - 완료: 메모리 2차 — EXIF 방향 5~8 JPEG은 디코더가 변환한 행 묶음을 회전된 위치에 바로 씀(`DecodeOptions::orientation`,
     순차·프로그레시브 모두; 스캔 뒤 세그먼트가 방향을 바꾸면 일반 디코딩으로 되돌림). 3000px 회전 JPEG 정화 54→34 MB,
     Pillow 일치 테스트 83개×방향 4개 동일. OCR 레이아웃의 `pixBlockconv`를 32 bpp 누적 표 대신 열 합을 행마다 옮기는
     제자리 계산으로(값 동일, 참조 구현과 비교 테스트), 디코딩 버퍼는 평면을 만든 즉시 해제: 3000px OCR 120→54 MB.
     PNG 병렬 압축은 앞 조각이 끝나는 대로 출력에 쓰고 해제, 조각·출력 버퍼는 실제 크기에 맞춤.
   - 고침: PNG 병렬 압축 출력이 실행마다 달라지던 문제(화소는 같음). 스레드별로 재사용하는 deflate 상태를 `reset`할 때
     창·`prev` 표·이미지 전략 비용 표가 남아 있었고, zlib-ng 매치 탐색은 입력 끝 너머 창 바이트까지 비교하므로 어느
     스레드가 어떤 조각을 맡느냐에 따라 결과가 달라짐. 이제 재사용 상태가 새 상태와 바이트 단위로 같은 출력을 냄(회귀 테스트).
   - 남음: 메타데이터 C2PA(JUMBF) 판독, 프로그레시브 JPEG 디코딩 메모리
4. 오디오 도구(ffmpeg/ffprobe) 포팅은 별도 저장소에서 같은 원칙으로 진행하고, 공통 크레이트(`photo-core`, `photo-deflate`)를 공유

## 벤치마크

`audeniq-photo-bench --iterations 5 --threads 4` (4 vCPU Intel Xeon 2.1 GHz, AVX-VNNI). 외부 측정은 자식 프로세스의
`ru_maxrss`/CPU, Rust는 프로세스 전체 VmHWM(측정마다 초기화)이라 Rust 쪽 RSS에는 벤치 프로세스 자체와 입력 버퍼가 포함됩니다.

### 기본 빌드 (x86-64, 런타임 AVX2/AVX-VNNI 선택; 2026-10 최적화 후)

| file | operation | Rust wall ms | Rust CPU ms | Rust peak RSS MB | external wall ms | external CPU ms | external peak RSS MB | speed-up |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| cover_3000.jpg | probe | 0 µs | 1 µs | 88.0 | 53.2 | 53.0 | 87.9 | 113626.2× |
| cover_3000.jpg | color | 1 µs | 1 µs | 88.0 | 99.0 | 98.9 | 87.9 | 73006.6× |
| cover_3000.jpg | provenance | 1 µs | 2 µs | 88.0 | 101.4 | 101.1 | 87.9 | 84223.3× |
| cover_3000.jpg | qr | 107.8 | 107.8 | 51.3 | 549.7 | 549.4 | 90.8 | 5.1× |
| cover_3000.jpg | cover (all of the above) | 108.3 | 108.3 | 51.0 | 810.2 | 807.5 | 90.8 | 7.5× |
| cover_3000.jpg | sanitize | 146.5 | 146.5 | 60.6 | 281.5 | 279.6 | 120.4 | 1.9× |
| cover_3000.png | probe | 577 µs | 578 µs | 33.6 | 248.1 | 247.8 | 94.0 | 429.8× |
| cover_3000.png | color | 3 µs | 4 µs | 33.6 | 101.1 | 100.9 | 33.5 | 34497.0× |
| cover_3000.png | provenance | 3 µs | 3 µs | 33.6 | 97.7 | 97.5 | 33.5 | 38677.2× |
| cover_3000.png | qr | 179.1 | 179.1 | 86.0 | 659.9 | 659.6 | 91.1 | 3.7× |
| cover_3000.png | cover (all of the above) | 185.7 | 185.7 | 82.4 | 1115.9 | 1115.1 | 93.9 | 6.0× |
| cover_3000.png | sanitize | 311.5 | 778.0 | 89.8 | 2889.3 | 2888.7 | 120.3 | 9.3× |
| cover_1400.jpg | probe | 1 µs | 1 µs | 93.9 | 48.1 | 47.9 | 93.7 | 61387.9× |
| cover_1400.jpg | color | 2 µs | 3 µs | 93.9 | 95.6 | 95.4 | 93.7 | 42538.4× |
| cover_1400.jpg | provenance | 2 µs | 2 µs | 93.9 | 99.4 | 99.2 | 95.4 | 61980.4× |
| cover_1400.jpg | qr | 19.8 | 19.8 | 36.8 | 124.9 | 124.7 | 36.6 | 6.3× |
| cover_1400.jpg | cover (all of the above) | 20.6 | 20.7 | 36.8 | 375.6 | 374.5 | 49.1 | 18.2× |
| cover_1400.jpg | sanitize | 28.3 | 28.2 | 32.2 | 118.9 | 118.0 | 39.7 | 4.2× |
| cover_1400_adobergb.jpg | probe | 2 µs | 3 µs | 32.2 | 46.4 | 46.2 | 49.1 | 20966.4× |
| cover_1400_adobergb.jpg | color | 6 µs | 7 µs | 32.2 | 101.1 | 100.9 | 31.9 | 17591.0× |
| cover_1400_adobergb.jpg | provenance | 3 µs | 3 µs | 32.2 | 107.0 | 106.7 | 31.9 | 33483.1× |
| cover_1400_adobergb.jpg | qr | 19.6 | 19.6 | 33.3 | 115.6 | 115.4 | 33.1 | 5.9× |
| cover_1400_adobergb.jpg | cover (all of the above) | 19.2 | 19.2 | 33.3 | 377.9 | 376.5 | 49.3 | 19.7× |
| cover_1400_adobergb.jpg | sanitize | 41.0 | 41.0 | 32.4 | 178.6 | 178.1 | 40.1 | 4.4× |
| signature.png | sanitize | 2.4 | 2.4 | 32.4 | 86.0 | 85.3 | 32.1 | 35.4× |
| document_scan_3p.pdf | pdf sanitize (vs pdfinfo+pdftoppm+rebuild) | 187.9 | 187.9 | 80.2 | 360.1 | 359.7 | 77.7 | 1.9× |
| document_text_2p.pdf | pdf sanitize (vs pdfinfo+pdftoppm+rebuild) | 69.1 | 69.1 | 59.4 | 163.5 | 163.0 | 59.1 | 2.4× |

Sanitize throughput with 4 threads: Rust 18.5 files/s, Python/Pillow 5.5 files/s (3.3×)

| file | sanitized size (Rust) | sanitized size (Python/Pillow) |
|---|---:|---:|
| cover_3000.jpg | 4353 KiB | 4353 KiB |
| cover_3000.png | 10979 KiB | 10529 KiB |
| cover_1400.jpg | 730 KiB | 730 KiB |
| cover_1400_adobergb.jpg | 866 KiB | 866 KiB |
| signature.png | 2 KiB | 2 KiB |

Rust peak RSS is the whole benchmark process (VmHWM, reset before each run); external figures are the child's ru_maxrss. CPU is user+system time.

이전 측정(최적화 전)과 비교: 3000px JPEG 정화 285 → 146 ms, 커버 종합 검사 3000px JPEG 163 → 108 ms,
3000px PNG 정화 0.71 → 0.31 s, PDF 3쪽 스캔 263 → 188 ms. Rust CPU 열은 작업자 스레드를 포함한 프로세스 전체
CPU(getrusage)라 병렬 압축하는 PNG 정화는 벽시계보다 큽니다. 정화 처리량(4스레드)은 18.5 files/s로 이전(19.0)과
측정 오차 안에서 같습니다(병렬 압축은 지연을 줄이지만 전체 CPU는 줄이지 않음). 이 VM의 같은 측정 반복 편차는 약 10%입니다. 런타임 선택이 생겨 `-C target-cpu=x86-64-v3` 빌드는
더 이상 필요하지 않습니다.
