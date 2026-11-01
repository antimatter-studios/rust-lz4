//! What goes in comes back, through this crate's decoder and an independent
//! one, at every level, and fixed-output compression fills what it is given.

use lz4::{compress_hc, compress_hc_dest_size, decompress, DecodeError};

fn text(len: usize, seed: u64) -> Vec<u8> {
    const WORDS: [&str; 16] = [
        "system", "vendor", "service", "class", "user", "group", "oneshot", "disabled", "socket",
        "property", "trigger", "start", "write", "chmod", "0755", "/data",
    ];
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(WORDS[(state % 16) as usize].as_bytes());
        out.push(if state.is_multiple_of(7) { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn samples() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for len in [
        0, 1, 4, 5, 12, 13, 14, 15, 16, 17, 64, 255, 256, 4095, 4096, 4097, 70_000, 300_001,
    ] {
        out.push((format!("text{len}"), text(len, len as u64)));
        out.push((format!("noise{len}"), noise(len, len as u64)));
        out.push((format!("zeros{len}"), vec![0u8; len]));
    }
    let mut mixed = text(100_000, 3);
    mixed.extend(noise(70_003, 4));
    mixed.extend(vec![7u8; 90_000]);
    mixed.extend(text(50_000, 5));
    out.push(("mixed".into(), mixed));
    out
}

/// The format's own rules on the last sequence, read from the block.
fn assert_end_rules(label: &str, block: &[u8], len: usize) {
    let (mut at, mut out) = (0usize, 0usize);
    let mut last_match_start = None;
    loop {
        let token = block[at];
        at += 1;
        let mut literals = (token >> 4) as usize;
        if literals == 15 {
            loop {
                let b = block[at];
                at += 1;
                literals += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        at += literals;
        out += literals;
        if at == block.len() {
            assert!(
                len < 5 || literals >= 5,
                "{label}: last literals {literals}"
            );
            if let Some(start) = last_match_start {
                assert!(len - start >= 12, "{label}: last match starts at {start}");
            }
            return;
        }
        at += 2;
        let mut length = (token & 15) as usize;
        if length == 15 {
            loop {
                let b = block[at];
                at += 1;
                length += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        last_match_start = Some(out);
        out += length + 4;
    }
}

#[test]
fn every_sample_round_trips_at_every_level() {
    for (name, data) in samples() {
        if data.len() > 100_000 && !name.starts_with("mixed") {
            continue;
        }
        for level in 1..=12u8 {
            let block = compress_hc(&data, level);
            let label = format!("{name} level {level}");
            assert!(decompress(&block, data.len()).unwrap() == data, "{label}");
            assert!(
                lz4_flex::block::decompress(&block, data.len()).unwrap() == data,
                "{label}: lz4_flex"
            );
            assert_end_rules(&label, &block, data.len());
        }
    }
}

#[test]
fn liblz4_reads_every_block() {
    for (name, data) in samples() {
        let block = compress_hc(&data, 12);
        let mut out = vec![0u8; data.len()];
        let n = unsafe {
            lz4_sys::LZ4_decompress_safe(
                block.as_ptr().cast(),
                out.as_mut_ptr().cast(),
                block.len() as i32,
                out.len() as i32,
            )
        };
        assert_eq!(n as usize, data.len(), "{name}");
        assert!(out == data, "{name}");
    }
}

#[test]
fn a_fixed_output_takes_what_fits_and_decodes_back() {
    for (name, data) in samples() {
        for size in [1usize, 2, 16, 512, 32768, 1 << 20] {
            for level in [1u8, 12] {
                let mut dst = vec![0u8; size];
                let (taken, written) = compress_hc_dest_size(&data, &mut dst, level);
                let label = format!("{name} into {size} level {level}");
                assert!(written <= size, "{label}: wrote {written}");
                assert!(taken <= data.len(), "{label}: took {taken}");
                let back = decompress(&dst[..written], taken).unwrap();
                assert!(back == data[..taken], "{label}");
                assert!(
                    lz4_flex::block::decompress(&dst[..written], taken).unwrap() == data[..taken],
                    "{label}: lz4_flex"
                );
                assert_end_rules(&label, &dst[..written], taken);
                if size >= lz4::compress_bound(data.len()) {
                    assert_eq!(taken, data.len(), "{label}");
                }
            }
        }
    }
}

#[test]
fn a_full_output_is_nearly_filled() {
    let data = text(400_000, 9);
    let mut dst = vec![0u8; 32768];
    let (taken, written) = compress_hc_dest_size(&data, &mut dst, 12);
    assert!(written > 32768 - 64, "wrote {written}");
    assert!(taken > 32768 && taken < data.len(), "took {taken}");
}

#[test]
fn an_empty_output_takes_nothing() {
    assert_eq!(compress_hc_dest_size(b"data", &mut [], 12), (0, 0));
}

#[test]
fn broken_blocks_are_refused_not_panicked_over() {
    assert_eq!(decompress(&[], 16), Err(DecodeError::Truncated));
    assert_eq!(decompress(&[0x50, 1, 2], 16), Err(DecodeError::Truncated));
    assert_eq!(
        decompress(&[0x10, 1, 0, 0], 16),
        Err(DecodeError::BadOffset)
    );
    assert_eq!(
        decompress(&[0x10, 1, 9, 0], 16),
        Err(DecodeError::BadOffset)
    );
    assert_eq!(
        decompress(&[0x20, 1, 2], 1),
        Err(DecodeError::OutputTooSmall)
    );
    assert_eq!(decompress(&[0xF0, 255], 16), Err(DecodeError::Truncated));
    let mut state = 7u64;
    for len in 0..2000usize {
        let junk = noise(len % 97, state);
        state = state.wrapping_add(11);
        let _ = decompress(&junk, 4096);
    }
}

/// Within 2% of liblz4's high-compression mode on what a system partition
/// holds, at the fixed output a filesystem's physical cluster asks for.
#[test]
fn the_ratio_is_within_two_percent_of_liblz4() {
    extern "C" {
        fn LZ4_sizeofStateHC() -> i32;
        fn LZ4_compress_HC_destSize(
            state: *mut std::ffi::c_void,
            src: *const std::os::raw::c_char,
            dst: *mut std::os::raw::c_char,
            src_size: *mut i32,
            target: i32,
            level: i32,
        ) -> i32;
    }
    let mut data = Vec::new();
    for i in 0..40u64 {
        data.extend(text(9000 + i as usize * 311, i));
        data.extend(noise(3000, i + 100));
        data.extend(text(30_000, i + 200));
    }
    type Compress<'a> = &'a mut dyn FnMut(&[u8], &mut [u8]) -> (usize, usize);
    let run = |compress: Compress| {
        let (mut pos, mut total) = (0usize, 0usize);
        let mut dst = vec![0u8; 32768];
        while pos < data.len() {
            let end = data.len().min(pos + (1 << 20));
            let (taken, written) = compress(&data[pos..end], &mut dst);
            assert!(taken > 0);
            pos += taken;
            total += written;
        }
        total
    };
    let ours = run(&mut |src, dst| compress_hc_dest_size(src, dst, 12));
    let mut state = vec![0u64; unsafe { LZ4_sizeofStateHC() } as usize / 8 + 1];
    let theirs = run(&mut |src, dst| {
        let mut taken = src.len() as i32;
        let written = unsafe {
            LZ4_compress_HC_destSize(
                state.as_mut_ptr().cast(),
                src.as_ptr().cast(),
                dst.as_mut_ptr().cast(),
                &mut taken,
                dst.len() as i32,
                12,
            )
        };
        (taken as usize, written as usize)
    });
    println!(
        "ours {ours}, liblz4 {theirs}: {:+.2}%",
        (ours as f64 / theirs as f64 - 1.0) * 100.0
    );
    assert!(
        ours as f64 <= theirs as f64 * 1.02,
        "ours {ours}, liblz4 {theirs}"
    );
}
