# rust-lz4

Pure-Rust **LZ4 block** compressor and decompressor, with a high-compression
mode and fixed-output compression. No C bindings, no `unsafe`, no dependencies.

```rust
let packed = lz4::compress_hc(&data, 12);
let data = lz4::decompress(&packed, data.len())?;

// As much of `data` as fits in `out`: what filling a filesystem's fixed-size
// physical cluster needs.
let (taken, written) = lz4::compress_hc_dest_size(&data, &mut out, 12);
```

The output is standard LZ4 block format, which any LZ4 decoder reads, including
the Linux kernel's. It implements the block format only, not the LZ4 frame
format.

## Provenance

Written from the published
[LZ4 block format description](https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md).
It carries no code from any other LZ4 implementation. The tests compare against
liblz4 and `lz4_flex` as independent references, and neither reaches a consumer
of this crate.
