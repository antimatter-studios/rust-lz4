//! Pure-Rust LZ4 block format: a decoder, and a high-compression encoder that
//! can fill a fixed-size output.
//!
//! # The block format
//!
//! A block is a series of sequences. Each starts with a token byte: its high
//! nibble is the literal count, and its low nibble is the match length less
//! four. A nibble of 15 means the count continues in the following bytes, each
//! adding its value, until one is below 255. Then come the literals, a
//! little-endian 16-bit offset back into the output, and any match length
//! bytes. The last sequence is literals alone. The last five bytes of a block
//! are always literals, and the last match starts at least twelve bytes before
//! the end.
//!
//! # Provenance
//!
//! Written from the published block format description. It carries no code
//! from any other LZ4 implementation.
#![forbid(unsafe_code)]

mod decode;
mod encode;

pub use decode::{decompress, decompress_into, DecodeError};
pub use encode::{compress_bound, compress_hc, compress_hc_dest_size, HcEncoder};

/// The highest compression level.
pub const MAX_LEVEL: u8 = 12;
