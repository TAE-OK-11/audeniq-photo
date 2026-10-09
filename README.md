# audeniq-photo

Audeniq 백엔드가 외부 프로세스로 실행하던 이미지 도구(ffprobe, ExifTool, ZBar,
Python/Pillow/LittleCMS 업로드 정화기, Poppler `pdfinfo`/`pdftoppm`)를 **Rust로 포팅해 하나의 라이브러리**로 합친 저장소입니다.
원칙: **이미 Rust로 된 우수한 구현은 가져와 Audeniq에 맞게 개선**하고(zlib-rs, hayro), **Rust가 아닌 도구는 포팅**합니다.
`unsafe`는 `photo-deflate`의 엔진 모듈(zlib-rs에서 합친 SIMD 커널·스트림 버퍼), `photo-pdf-syntax`의 페이지 캐시 한 곳,
`photo-ocr`의 AVX2 커널 호출 한 곳(CPU 감지 뒤 같은 안전 코드를 AVX2로 컴파일한 함수를 부름)에만 있고 나머지 크레이트는 `unsafe`를 금지합니다.
외부 C 라이브러리나 실행 파일에는 의존하지 않습니다(포팅 현황 — [docs/PORTING.md](docs/PORTING.md)).

## 구성

| 크레이트 | 역할 | 포팅 원본 |
|---|---|---|
| `photo-core` | 공통 오류·자원 한도·데드라인·픽셀 형식 | — |
| `photo-deflate` | zlib/DEFLATE: zlib-rs 엔진을 합쳐 직접 호출(버퍼 직접 기록, 스레드별 상태 재사용, CPU 판별 1회), 출력 한도·절단/정확 모드 | [zlib-rs](https://github.com/trifectatechfoundation/zlib-rs) 0.6.8 (`ENGINE.md`) |
| `photo-png` | PNG 디코더(전 색상형·비트 깊이·Adam7), 스트리밍 인코더, 전자서명 PNG 엄격 검증 | libpng 동작, `sanitize-upload.py` |
| `photo-jpeg` | JPEG 디코더(베이스라인·프로그레시브), 베이스라인 인코더 | libjpeg-turbo (ISLOW IDCT, fancy 업샘플링, jdcolor, jcdctmgr) |
| `photo-icc` | ICC 파서, sRGB 변환(매트릭스-셰이퍼·LUT·CMYK, BPC) | LittleCMS 2 |
| `photo-meta` | EXIF/XMP/ICC/PNG 텍스트, RIFF·AIFF·FLAC·MP4·WavPack·TTA 오디오 태그 | ExifTool 리더 |
| `photo-qr` | QR 검출·디코딩(디코딩 성공 개수) | quirc + 표준 Reed–Solomon |
| `photo-pdf` 외 7개 (`-syntax`, `-interpret`, `-ccitt`, `-jbig2`, `-jpeg2000`, `-cmap`, `-postscript`) | PDF 파서·인터프리터·래스터라이저. Flate는 `photo-deflate`, JPEG는 `photo-jpeg`로 연결 | [hayro](https://github.com/LaurenzV/hayro) 0.8.0 (`crates/photo-pdf/UPSTREAM.md`) |
| `photo-ocr` | OCR: Tesseract 포팅 — 이진화·선 제거·희소 텍스트 배치 분석(`--psm 11`), 기준선·행·단어 분할, LSTM 인식(int8 망, CTC 빔 탐색+사전), 언어 재시도, TSV 출력. `tessdata_fast` eng/kor 모델 내장 | Tesseract 5.3.4, Leptonica 1.82 |
| `audeniq-photo` | 백엔드용 통합 API: `probe`, `color_report`, `provenance_fields`, `qr_count`, `ocr_tsv`, `sanitize`, `pdf`, `inspect_cover` | — |
| `photo-cli` | `audeniq-photo` 명령행 도구 | — |
| `photo-bench` | `audeniq-photo-bench` 외부 도구 대비 벤치마크 | — |

## 원본과의 일치 검증 (테스트로 고정)

- JPEG 디코딩: Pillow(libjpeg-turbo)와 **비트 단위 동일** (83개 조합: 4:4:4/4:2:2/4:2:0/4:4:0/4:1:1, 프로그레시브, 재시작 마커, 흑백, CMYK)
- JPEG 인코딩: 정화기 설정(q95 4:4:4, q85 4:2:0)에서 Pillow 출력과 **바이트 단위 동일**
- 업로드 정화: 원본 `sanitize-upload.py` 대비 JPEG 출력 바이트 동일, PNG 픽셀 동일, PDF 바이트 동일, ICC 변환 ±1~3
- ICC: LittleCMS 대비 RGB ±2, CMYK ±3, Gray 동일 (시스템 프로필 22종)
- 메타데이터: ExifTool `-j -n -s` 색상 JSON 동일, `-G1` 출처 태그는 상위 집합(ExifTool이 못 읽는 AIFF ID3·WavPack/TTA APE까지 읽음)
- QR: zbarimg와 디코딩 개수 동일 (버전 1~40, 회전, 다중 코드, 잡음 배경, 3000px 커버)
- PDF: hayro·pdf.js·PDFBox 시험 문서 395개에서 패닉·시간 초과 없음, 첫 페이지 271개 중 196개가 pdftoppm과 평균 차 2 미만
  (나머지 대부분은 Poppler가 그리지 못하는 Type3 글꼴·셰이딩 배경·이름 있는 색공간, 또는 MediaBox 대신 CropBox를 그리는 차이)
- OCR 한 줄 인식(`tesseract --psm 13`): 영어·한국어, 흑백·컬러·반전·잡음·흐림·9~90pt 151개 줄 이미지에서 단어와 신뢰도(소수 6자리)까지 **동일**
- OCR 페이지(`tesseract -l eng+kor --psm 11 tsv`): 생성 커버 172개(PNG 132, JPEG 40; 회전 글자·그라데이션·잡음·흐림·흑백 포함)에서 TSV **바이트 단위 동일**
  (블록·문단·줄·단어 상자, 신뢰도, 글자). 단일 스레드 기준 Tesseract보다 1.5배 빠르고 최대 메모리 48 MB(Tesseract 58 MB)
- 퍼징: 손상·절단 입력 수천 건에서 패닉 없음(패닉은 `Error::Internal`로 격리)

비교 테스트는 python3/Pillow, exiftool, zbarimg, qrencode, ffmpeg, poppler가 있을 때만 실행되고 없으면 건너뜁니다.

## 사용

```text
cargo build --release
target/release/audeniq-photo probe cover.jpg
target/release/audeniq-photo color cover.jpg
target/release/audeniq-photo provenance master.wav
target/release/audeniq-photo qr cover.png
target/release/audeniq-photo cover cover.jpg          # 한 번 읽어 모두
target/release/audeniq-photo sanitize in.png out.png image/png
target/release/audeniq-photo sanitize in.pdf out.pdf application/pdf   # Poppler 없이
target/release/audeniq-photo pdf-info in.pdf
target/release/audeniq-photo pdf-render in.pdf 1 page1.png
target/release/audeniq-photo pdf-rasterize in.pdf pages.frames    # 샌드박스 안(신뢰 불가 단계)
target/release/audeniq-photo pdf-assemble pages.frames out.pdf     # 밖(픽셀 검증 후 PDF 작성)
target/release/audeniq-photo-bench --reference-sanitizer crates/audeniq-photo/tests/reference/sanitize-upload.py
```

라이브러리:

```rust
let report = audeniq_photo::inspect_cover(&bytes, &Deadline::after(Duration::from_secs(30)))?;
let clean = audeniq_photo::sanitize(&bytes, Kind::Jpeg, &deadline)?;
let meta = audeniq_photo::metadata_file(Path::new("master.wav"))?; // 오디오 데이터는 읽지 않음
```

## 성능 요약

자세한 표는 [docs/PORTING.md](docs/PORTING.md#벤치마크). 4 vCPU Xeon 2.1 GHz, 기본(x86-64) 빌드
(AVX2·AVX-VNNI 경로는 실행 중 CPU를 보고 선택):

| 작업 | 기존(외부 프로세스) | Rust | |
|---|---:|---:|---|
| 커버 종합 검사 3000px JPEG (probe+색상+출처+QR) | 810 ms / 91 MB | 108 ms | 7.5× |
| 커버 종합 검사 1400px JPEG | 376 ms | 21 ms | 18× |
| 3000px JPEG 정화 | 282 ms / 120 MB | 146 ms / 61 MB | 1.9× |
| 3000px PNG 정화 (4코어 병렬 압축, CPU 0.78 s) | 2.89 s | 0.31 s | 9.3× |
| 1400px JPEG(Adobe RGB) 정화 | 179 ms | 41 ms | 4.4× |
| PDF 정화 3쪽 스캔 문서 (pdfinfo+pdftoppm+재작성) | 360 ms | 188 ms | 1.9× |
| PDF 정화 2쪽 텍스트 문서 | 164 ms | 69 ms | 2.4× |
| 정화 처리량(4스레드) | 5.5 files/s | 18.5 files/s | 3.3× |
| OCR 52개 표지 (`tesseract -l eng+kor --psm 11`, 단일 스레드) | 23.9 s / 58 MB | 7.4 s / 45 MB | 3.2× |

구성 요소별 최적화(같은 출력 유지):

| 구성 요소 | 이전 | 이후 | 참고 |
|---|---:|---:|---|
| JPEG 인코딩 3000px q95 4:4:4 | 220 ms | 80 ms | libjpeg-turbo 59 ms, 바이트 동일 |
| JPEG 디코딩 3000px | 93 ms | 77 ms | libjpeg-turbo 83 ms, 비트 동일 |
| PNG 인코딩 3000px 사진형 | 585 ms / 12.0 MB | 220 ms / 10.7 MB | Pillow 5.1 s / 11.4 MB, 화소 동일 |
| QR 3000px | 132 ms | 71 ms | zbar 비교 동일 |
| OCR 표지당 (배치 분석 + 인식) | 270 ms | 142 ms | TSV 바이트 동일 |
| JPEG 디코딩 최대 힙 3000px 4:2:0 / 4:4:4 | 40.6 / 54.0 MB | 27.2 / 27.2 MB | MCU 행 스트리밍, 비트 동일 (프로그레시브 제외) |
| PDF 셰이딩 페이지 (PostScript 함수) | 1.4~3.1 s | 0.6~1.6 s | 레지스터 코드 컴파일, 래스터 동일 |
| PNG 인코딩 3000px zlib 경로 / 사진형 (4코어) | 636 / 224 ms | 208 / 78 ms | 병렬 조각 압축, 화소 동일, 크기 ±0.1% 안 |
| 프로그레시브 JPEG 디코딩 3000px | 195 ms | 168 ms | 지그재그 계수·비트마스크 정밀화, 비트 동일 (libjpeg-turbo 152 ms) |
| EXIF 회전(6) 3000px JPEG 정화 | 194 ms | 169 ms | 타일 단위 한 번 복사, 뒤집기는 제자리 |
| CMYK JPEG 3000px 커버 검사 최대 힙 | 72 MB | 45 MB | 휘도를 디코딩 버퍼 안에서 계산 |
| PDF 정화 3쪽 스캔 / 2쪽 텍스트 | 212 / 79 ms | 166 / 57 ms | 다음 쪽 렌더링과 이전 쪽 JPEG 인코딩을 겹침, 출력 바이트 동일, 최대 힙 +11~13 MB(래스터 한 장) |

런타임 SIMD는 `AUDENIQ_PHOTO_NO_SIMD=1`(전부)과 `AUDENIQ_PHOTO_NO_VNNI=1`(AVX-VNNI만)로 끌 수 있고, 어느 경로든 결과는 같습니다.
