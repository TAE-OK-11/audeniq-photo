# audeniq-photo

Audeniq 백엔드가 외부 프로세스로 실행하던 이미지 도구(ffprobe, ExifTool, ZBar,
Python/Pillow/LittleCMS 업로드 정화기)를 **Rust로 포팅해 하나의 라이브러리**로 합친 저장소입니다.
원칙: **이미 Rust로 된 우수한 구현은 가져와 Audeniq에 맞게 개선**하고(zlib-rs), **Rust가 아닌 도구는 포팅**합니다.
`unsafe`는 `photo-deflate`의 엔진 모듈(zlib-rs에서 합친 SIMD 커널·스트림 버퍼)에만 있고, 나머지 크레이트는 `#![forbid(unsafe_code)]`입니다.
외부 C 라이브러리나 실행 파일에는 의존하지 않습니다(Poppler·Tesseract는 다음 단계 — [docs/PORTING.md](docs/PORTING.md)).

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
| `audeniq-photo` | 백엔드용 통합 API: `probe`, `color_report`, `provenance_fields`, `qr_count`, `sanitize`, `pdf`, `inspect_cover` | — |
| `photo-cli` | `audeniq-photo` 명령행 도구 | — |
| `photo-bench` | `audeniq-photo-bench` 외부 도구 대비 벤치마크 | — |

## 원본과의 일치 검증 (테스트로 고정)

- JPEG 디코딩: Pillow(libjpeg-turbo)와 **비트 단위 동일** (83개 조합: 4:4:4/4:2:2/4:2:0/4:4:0/4:1:1, 프로그레시브, 재시작 마커, 흑백, CMYK)
- JPEG 인코딩: 정화기 설정(q95 4:4:4, q85 4:2:0)에서 Pillow 출력과 **바이트 단위 동일**
- 업로드 정화: 원본 `sanitize-upload.py` 대비 JPEG 출력 바이트 동일, PNG 픽셀 동일, PDF 바이트 동일, ICC 변환 ±1~3
- ICC: LittleCMS 대비 RGB ±2, CMYK ±3, Gray 동일 (시스템 프로필 22종)
- 메타데이터: ExifTool `-j -n -s` 색상 JSON 동일, `-G1` 출처 태그는 상위 집합(ExifTool이 못 읽는 AIFF ID3·WavPack/TTA APE까지 읽음)
- QR: zbarimg와 디코딩 개수 동일 (버전 1~40, 회전, 다중 코드, 잡음 배경, 3000px 커버)
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
target/release/audeniq-photo-bench --reference-sanitizer crates/audeniq-photo/tests/reference/sanitize-upload.py
```

라이브러리:

```rust
let report = audeniq_photo::inspect_cover(&bytes, &Deadline::after(Duration::from_secs(30)))?;
let clean = audeniq_photo::sanitize(&bytes, Kind::Jpeg, &deadline)?;
let meta = audeniq_photo::metadata_file(Path::new("master.wav"))?; // 오디오 데이터는 읽지 않음
```

## 성능 요약

자세한 표는 [docs/PORTING.md](docs/PORTING.md#벤치마크). 4 vCPU Xeon 2.1 GHz, 기본(x86-64) 빌드:

| 작업 | 기존(외부 프로세스) | Rust | |
|---|---:|---:|---|
| 커버 종합 검사 3000px JPEG (probe+색상+출처+QR) | 700 ms / 91 MB | 152 ms / 41 MB | 4.6× |
| 커버 종합 검사 1400px JPEG | 316 ms | 31 ms | 10× |
| 3000px PNG 정화 | 2.60 s | 0.73 s | 3.5× |
| 1400px JPEG(Adobe RGB) 정화 | 148 ms | 61 ms | 2.4× |
| 정화 처리량(4스레드) | 6.1 files/s | 17.6 files/s | 2.9× |

`-C target-cpu=x86-64-v3`(백엔드 Dockerfile의 `TARGET_CPU`) 빌드에서는 3000px JPEG 정화가 1.4×, 처리량 2.4×입니다.
