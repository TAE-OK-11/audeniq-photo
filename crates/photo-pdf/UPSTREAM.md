# photo-pdf 계열 크레이트의 원본

`photo-pdf*` 크레이트는 순수 Rust PDF 렌더러 [hayro](https://github.com/LaurenzV/hayro)
0.8.0 (커밋 `ea9c81dc70cde40c64205986fdcfcd481fe934d8`, Apache-2.0 OR MIT)를 가져와 audeniq-photo 안으로 합친 것입니다.
라이선스 원문은 `LICENSE-MIT-hayro`, `LICENSE-APACHE-hayro`, 출처 고지는 `NOTICE-hayro.md`에 있습니다.

| 이 저장소 | hayro |
|---|---|
| `photo-pdf` | `hayro` (래스터라이저 연결) |
| `photo-pdf-syntax` | `hayro-syntax` (xref·객체·필터·암호화) |
| `photo-pdf-interpret` | `hayro-interpret` (콘텐츠 스트림·글꼴·색공간·셰이딩) |
| `photo-pdf-ccitt` / `-jbig2` / `-jpeg2000` | `hayro-ccitt` / `-jbig2` / `-jpeg2000` |
| `photo-pdf-cmap` / `-postscript` | `hayro-cmap` / `-postscript` |

## 바꾼 점

- **Flate**: `flate2` 대신 `photo-deflate`(합쳐진 zlib-rs 엔진)를 직접 호출. 스트림당 256 MiB 출력 한도
  (압축 폭탄 방지)를 엔진과 pdf.js식 복구 디코더 모두에 적용.
- **DCT(JPEG)**: `zune-jpeg` 대신 `photo-jpeg`(libjpeg-turbo 포팅). PDF 의미에 맞게
  `DecodeOptions`(`/ColorTransform 0`이면 YCbCr 유지, CMYK는 저장값 그대로) 추가, 1.28억 픽셀 한도.
- **메시 셰이딩(유형 4–7)**: 장치 픽셀마다 해시맵에 저장하던 샘플을 칠해지는 영역으로 잘린 조밀 격자로 교체.
  같은 출력에서 Coons 패치 예제의 최대 메모리 1978 MB → 96 MB, 10.7 s → 2.9 s.
- `photo_pdf::render_rgb`: 정확한 픽셀 크기(pdftoppm `-scale-to`)로 흰 배경 RGB8을 직접 생성.
- `XRef::is_encrypted` 추가(정화기의 암호화 문서 거부용).
- `image` 크레이트 연동(jbig2/jpeg2000 `integration.rs`), `unsafe` 기능 플래그(flate2·zune SIMD 선택), 예제·벤치·외부 자산 테스트 제거.
  파일을 읽던 단위 테스트는 생성한 PDF로 대체.
- `unsafe`: `photo-pdf-syntax`의 자기참조 페이지 캐시(`page.rs` `CachedPages`) 한 곳만 `#[allow(unsafe_code)]`, 나머지는 금지.

## 아직 외부 크레이트를 쓰는 부분 (이후 합칠 후보)

`vello_cpu`(래스터라이저), `skrifa`(글꼴), `kurbo`(기하), `moxcms`(ICC — `photo-icc`로 대체 예정),
`pic-scale`(이미지 리샘플링), `fearless_simd`, `phf`, `yoke`, `smallvec`, `rustc-hash`, `brotli`, `memchr`.
