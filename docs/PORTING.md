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
| 7 | Tesseract | 커버마다 실행되지만 LSTM 엔진·eng/kor 모델 포팅은 별도 대형 과제 | 2~3차 |

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
(`pdf::image_only_pdf_from_frames`). 자식이 오염돼도 결과물에는 픽셀만 들어갑니다. Tesseract는 기존대로 샌드박스에서 실행됩니다.

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
2. **3차**: OCR — Tesseract LSTM 추론 엔진과 traineddata 로더 포팅, 커버 텍스트 검출 전처리 공유
3. 자체 개발 단계: 포팅 코드를 기준선으로 고정(현재의 비트 동일 테스트)한 뒤 SIMD 경로(target_feature)와 자체 매치파인더·허프만 최적화, 메타데이터 C2PA(JUMBF) 판독 추가
4. 오디오 도구(ffmpeg/ffprobe) 포팅은 별도 저장소에서 같은 원칙으로 진행하고, 공통 크레이트(`photo-core`, `photo-deflate`)를 공유

## 벤치마크

`audeniq-photo-bench --iterations 5 --threads 4` (4 vCPU Intel Xeon 2.1 GHz). 외부 측정은 자식 프로세스의
`ru_maxrss`/CPU, Rust는 프로세스 전체 VmHWM(측정마다 초기화)이라 Rust 쪽 RSS에는 벤치 프로세스 자체와 입력 버퍼가 포함됩니다.

### 기본 빌드 (x86-64, zlib-rs 적용 후)

| file | operation | Rust wall ms | Rust CPU ms | Rust peak RSS MB | external wall ms | external CPU ms | external peak RSS MB | speed-up |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| cover_3000.jpg | probe | 0 µs | 0 µs | 10.1 | 46.0 | 45.8 | 52.1 | 141143.3× |
| cover_3000.jpg | color | 1 µs | 0 µs | 10.1 | 86.1 | 85.9 | 18.7 | 94594.4× |
| cover_3000.jpg | provenance | 1 µs | 0 µs | 10.1 | 84.0 | 83.8 | 18.8 | 58627.3× |
| cover_3000.jpg | qr | 155.8 | 156.0 | 40.8 | 547.8 | 547.4 | 90.8 | 3.5× |
| cover_3000.jpg | cover (all of the above) | 163.4 | 164.0 | 40.8 | 710.6 | 709.3 | 90.7 | 4.3× |
| cover_3000.jpg | sanitize | 285.2 | 287.9 | 45.1 | 269.9 | 268.0 | 120.4 | 0.9× |
| cover_3000.png | probe | 713 µs | 0 µs | 45.1 | 225.7 | 225.5 | 94.4 | 316.8× |
| cover_3000.png | color | 3 µs | 0 µs | 45.1 | 91.2 | 91.0 | 44.6 | 29518.5× |
| cover_3000.png | provenance | 3 µs | 0 µs | 45.1 | 91.0 | 90.9 | 44.7 | 34088.8× |
| cover_3000.png | qr | 218.8 | 220.0 | 70.5 | 627.4 | 626.6 | 91.1 | 2.9× |
| cover_3000.png | cover (all of the above) | 228.6 | 228.0 | 70.5 | 1005.5 | 1004.8 | 94.3 | 4.4× |
| cover_3000.png | sanitize | 732.3 | 731.9 | 70.6 | 2596.6 | 2592.4 | 120.2 | 3.5× |
| cover_1400.jpg | probe | 0 µs | 0 µs | 45.1 | 43.2 | 42.9 | 49.7 | 162266.6× |
| cover_1400.jpg | color | 1 µs | 0 µs | 45.1 | 87.9 | 87.7 | 44.9 | 63036.3× |
| cover_1400.jpg | provenance | 1 µs | 0 µs | 45.1 | 88.0 | 87.8 | 44.9 | 104443.4× |
| cover_1400.jpg | qr | 31.9 | 32.0 | 45.1 | 102.1 | 102.0 | 44.9 | 3.2× |
| cover_1400.jpg | cover (all of the above) | 28.6 | 28.0 | 45.1 | 320.7 | 319.9 | 49.4 | 11.2× |
| cover_1400.jpg | sanitize | 49.8 | 48.0 | 45.1 | 98.4 | 97.7 | 44.9 | 2.0× |
| cover_1400_adobergb.jpg | probe | 1 µs | 0 µs | 45.1 | 46.3 | 46.1 | 49.5 | 33863.9× |
| cover_1400_adobergb.jpg | color | 6 µs | 0 µs | 45.1 | 96.6 | 96.4 | 44.9 | 15022.3× |
| cover_1400_adobergb.jpg | provenance | 3 µs | 0 µs | 45.1 | 85.9 | 85.8 | 45.0 | 25442.6× |
| cover_1400_adobergb.jpg | qr | 29.0 | 28.0 | 45.1 | 102.3 | 102.1 | 45.0 | 3.5× |
| cover_1400_adobergb.jpg | cover (all of the above) | 28.7 | 28.0 | 45.1 | 322.2 | 321.7 | 49.3 | 11.2× |
| cover_1400_adobergb.jpg | sanitize | 61.2 | 60.0 | 45.1 | 145.9 | 145.7 | 45.0 | 2.4× |
| signature.png | sanitize | 1.3 | 0 µs | 45.1 | 66.1 | 65.7 | 45.0 | 51.6× |

Sanitize throughput with 4 threads: Rust 17.6 files/s, Python/Pillow 6.1 files/s (2.9×)

| file | sanitized size (Rust) | sanitized size (Python/Pillow) |
|---|---:|---:|
| cover_3000.jpg | 4353 KiB | 4353 KiB |
| cover_3000.png | 10981 KiB | 10529 KiB |
| cover_1400.jpg | 730 KiB | 730 KiB |
| cover_1400_adobergb.jpg | 866 KiB | 866 KiB |
| signature.png | 2 KiB | 2 KiB |

Rust peak RSS is the whole benchmark process (VmHWM, reset before each run); external figures are the child's ru_maxrss. CPU is user+system time.

### `-C target-cpu=x86-64-v3` (zlib-rs 적용 전 측정)

| file | operation | Rust wall ms | Rust CPU ms | Rust peak RSS MB | external wall ms | external CPU ms | external peak RSS MB | speed-up |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| cover_3000.jpg | probe | 0 µs | 0 µs | 10.3 | 44.9 | 44.7 | 51.8 | 127900.7× |
| cover_3000.jpg | color | 1 µs | 0 µs | 10.3 | 90.6 | 90.4 | 18.7 | 91114.2× |
| cover_3000.jpg | provenance | 1 µs | 0 µs | 10.3 | 83.7 | 83.5 | 18.7 | 108819.1× |
| cover_3000.jpg | qr | 153.0 | 152.0 | 41.2 | 474.0 | 473.6 | 90.7 | 3.1× |
| cover_3000.jpg | cover (all of the above) | 152.6 | 152.0 | 41.2 | 714.2 | 713.5 | 90.7 | 4.7× |
| cover_3000.jpg | sanitize | 178.0 | 180.0 | 45.5 | 242.9 | 241.4 | 120.3 | 1.4× |
| cover_3000.png | probe | 7.1 | 8.0 | 45.5 | 226.4 | 226.3 | 93.8 | 31.8× |
| cover_3000.png | color | 3 µs | 0 µs | 45.5 | 95.8 | 95.7 | 45.3 | 37892.8× |
| cover_3000.png | provenance | 3 µs | 0 µs | 45.5 | 87.8 | 87.6 | 45.3 | 34499.4× |
| cover_3000.png | qr | 262.5 | 264.0 | 71.0 | 612.0 | 611.7 | 90.9 | 2.3× |
| cover_3000.png | cover (all of the above) | 271.3 | 272.0 | 71.1 | 1072.0 | 1071.1 | 93.8 | 4.0× |
| cover_3000.png | sanitize | 1052.9 | 1051.7 | 71.0 | 2606.9 | 2606.3 | 120.2 | 2.5× |
| cover_1400.jpg | probe | 0 µs | 0 µs | 45.5 | 46.2 | 46.0 | 49.3 | 115428.3× |
| cover_1400.jpg | color | 1 µs | 0 µs | 45.5 | 86.6 | 86.3 | 45.3 | 62635.1× |
| cover_1400.jpg | provenance | 2 µs | 0 µs | 45.5 | 83.6 | 83.4 | 45.4 | 52519.5× |
| cover_1400.jpg | qr | 28.1 | 28.0 | 45.5 | 97.2 | 97.0 | 45.4 | 3.5× |
| cover_1400.jpg | cover (all of the above) | 27.8 | 28.0 | 45.5 | 311.4 | 310.9 | 49.2 | 11.2× |
| cover_1400.jpg | sanitize | 34.9 | 36.0 | 45.5 | 97.4 | 97.3 | 45.4 | 2.8× |
| cover_1400_adobergb.jpg | probe | 1 µs | 0 µs | 45.5 | 41.1 | 41.0 | 49.1 | 46383.1× |
| cover_1400_adobergb.jpg | color | 4 µs | 0 µs | 45.5 | 90.3 | 90.1 | 45.4 | 20627.7× |
| cover_1400_adobergb.jpg | provenance | 4 µs | 0 µs | 45.5 | 86.6 | 86.4 | 45.4 | 22541.3× |
| cover_1400_adobergb.jpg | qr | 38.0 | 36.0 | 45.5 | 113.3 | 113.1 | 45.3 | 3.0× |
| cover_1400_adobergb.jpg | cover (all of the above) | 32.5 | 32.0 | 45.5 | 346.9 | 345.7 | 49.1 | 10.7× |
| cover_1400_adobergb.jpg | sanitize | 46.4 | 48.0 | 45.5 | 156.1 | 155.9 | 45.3 | 3.4× |
| signature.png | sanitize | 3.0 | 4.0 | 45.5 | 67.1 | 66.4 | 45.3 | 22.1× |

Sanitize throughput with 4 threads: Rust 15.5 files/s, Python/Pillow 6.3 files/s (2.4×)

| file | sanitized size (Rust) | sanitized size (Python/Pillow) |
|---|---:|---:|
| cover_3000.jpg | 4353 KiB | 4353 KiB |
| cover_3000.png | 10793 KiB | 10529 KiB |
| cover_1400.jpg | 730 KiB | 730 KiB |
| cover_1400_adobergb.jpg | 866 KiB | 866 KiB |
| signature.png | 2 KiB | 2 KiB |

Rust peak RSS is the whole benchmark process (VmHWM, reset before each run); external figures are the child's ru_maxrss. CPU is user+system time.
