# photo-zlib — vendored zlib-rs

- Upstream: <https://github.com/trifectatechfoundation/zlib-rs>, crate `zlib-rs` 0.6.8
- Commit: `7909c0fc48f5d29f6610770e31d6f0c924f9ec3b` (2026-09-17)
- License: Zlib (`LICENSE`, kept verbatim)

## Changes for audeniq-photo

- Package renamed to `photo-zlib`; standalone manifest (no upstream workspace).
- Default features: `std` only (Rust global allocator). The libc `malloc`
  allocator (`c-allocator`) stays available but is not built by default.
- Source files are unmodified; upstream unit tests (quickcheck) run with
  `cargo test -p photo-zlib`.
- Consumers use it only through `photo-deflate`, which adds output limits,
  truncation/exact modes and the workspace error type.

Sync procedure: copy `zlib-rs/src` from a new upstream commit over `src/`,
re-apply the manifest above, run `cargo test --workspace` (zlib interop,
PNG, sanitizer parity) and update the commit hash here.
