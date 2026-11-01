//! The block decoder.

use std::fmt;

/// Why a block could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The block ends inside a sequence.
    Truncated,
    /// A match offset is zero or reaches before the start of the output.
    BadOffset,
    /// The block decodes to more than the output has room for.
    OutputTooSmall,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "LZ4 block ends inside a sequence",
            Self::BadOffset => "LZ4 match offset is outside the output",
            Self::OutputTooSmall => "LZ4 block decodes to more than the output holds",
        })
    }
}

impl std::error::Error for DecodeError {}

/// Read a count continued in extra bytes after a nibble of 15.
fn read_length(src: &[u8], at: &mut usize, nibble: usize) -> Result<usize, DecodeError> {
    let mut length = nibble;
    if nibble == 15 {
        loop {
            let byte = *src.get(*at).ok_or(DecodeError::Truncated)?;
            *at += 1;
            length = length
                .checked_add(byte as usize)
                .ok_or(DecodeError::OutputTooSmall)?;
            if byte != 255 {
                break;
            }
        }
    }
    Ok(length)
}

/// Decode one block into `dst`, returning the bytes written.
pub fn decompress_into(src: &[u8], dst: &mut [u8]) -> Result<usize, DecodeError> {
    let (mut at, mut out) = (0usize, 0usize);
    loop {
        let token = *src.get(at).ok_or(DecodeError::Truncated)?;
        at += 1;
        let literals = read_length(src, &mut at, (token >> 4) as usize)?;
        let end = at.checked_add(literals).ok_or(DecodeError::Truncated)?;
        let input = src.get(at..end).ok_or(DecodeError::Truncated)?;
        let out_end = out
            .checked_add(literals)
            .ok_or(DecodeError::OutputTooSmall)?;
        dst.get_mut(out..out_end)
            .ok_or(DecodeError::OutputTooSmall)?
            .copy_from_slice(input);
        at = end;
        out = out_end;
        if at == src.len() {
            return Ok(out);
        }
        let bytes = src.get(at..at + 2).ok_or(DecodeError::Truncated)?;
        let offset = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        at += 2;
        if offset == 0 || offset > out {
            return Err(DecodeError::BadOffset);
        }
        let length = read_length(src, &mut at, (token & 15) as usize)?
            .checked_add(4)
            .ok_or(DecodeError::OutputTooSmall)?;
        let out_end = out.checked_add(length).ok_or(DecodeError::OutputTooSmall)?;
        if out_end > dst.len() {
            return Err(DecodeError::OutputTooSmall);
        }
        // Byte by byte: a match may overlap the bytes it is writing.
        for i in out..out_end {
            dst[i] = dst[i - offset];
        }
        out = out_end;
    }
}

/// Decode one block of at most `max_output` bytes.
pub fn decompress(src: &[u8], max_output: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = vec![0u8; max_output];
    let written = decompress_into(src, &mut out)?;
    out.truncate(written);
    Ok(out)
}
