# DEFLATE engine

`src/engine` is the compression engine of zlib-rs 0.6.8
(<https://github.com/trifectatechfoundation/zlib-rs>, commit
`7909c0fc48f5d29f6610770e31d6f0c924f9ec3b`, Zlib license in
`LICENSE-zlib-rs`), merged into audeniq-photo and maintained here.

Changes from upstream:

- Merged as a module of `photo-deflate` instead of a separate crate; the
  crate's own API (`src/stream.rs`) calls the engine directly: inflate and
  deflate write straight into the caller's `Vec` spare capacity (no
  zero-filled staging buffers, no 64 KiB scratch copy), and stream states are
  reused per thread instead of being allocated and initialized per stream.
- CPU features are detected once per process into one atomic profile
  (`engine/cpu_features.rs`); upstream queried at each SIMD call site.
  `audeniq_photo` services can call `photo_deflate::warm_up()` at start.
- Removed: the `stable` wrapper, the callback (`infback`) API, the C malloc
  allocator default, and the LoongArch/wasm back ends (targets are x86_64
  and aarch64).
- Algorithms, tables and SIMD kernels are unchanged, and the upstream unit
  tests still run (`cargo test -p photo-deflate`).
