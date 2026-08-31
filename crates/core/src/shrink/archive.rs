//! The cold-archive container: content-defined chunks, zstd with trained
//! dictionaries, one pack file per archive.
//!
//! A pack is an 8-byte magic followed by independently-compressed chunks,
//! back to back. All structure — which chunks make up which member, where
//! each chunk sits, which dictionary it used — lives in the database, so a
//! chunk that already exists in an *older* pack is never stored again
//! (`creditos-v1` and `creditos` cost one copy). Every member is decoded
//! and re-hashed against the original before the original is touched, and
//! restore verifies the same hash again on the way out.

use crate::{CoreError, Result};
use std::io::{Read, Seek, SeekFrom};

pub const PACK_MAGIC: &[u8; 8] = b"FMPACK1\0";
/// FastCDC bounds: 16 KiB / 64 KiB / 256 KiB. Small enough that edited
/// copies of a project share most chunks, large enough that zstd still has
/// context to work with.
pub const CHUNK_MIN: u32 = 16 * 1024;
pub const CHUNK_AVG: u32 = 64 * 1024;
pub const CHUNK_MAX: u32 = 256 * 1024;
/// zstd level for archives (matches the estimate's probe).
pub const LEVEL: i32 = 19;
/// Upper bound for a trained dictionary (zstd's recommended ~110 KiB).
pub const DICT_MAX: usize = 110 * 1024;
/// Fewer samples than this and training is skipped (unstable dictionaries).
pub const DICT_MIN_SAMPLES: usize = 8;
/// Cap on bytes fed to the trainer per category.
pub const DICT_SAMPLE_BYTES: usize = 8 * 1024 * 1024;

/// Byte ranges of one buffer, in order.
pub fn chunk_spans(data: &[u8]) -> Vec<(usize, usize)> {
    if data.is_empty() {
        return Vec::new();
    }
    fastcdc::v2020::FastCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX)
        .map(|c| (c.offset, c.length))
        .collect()
}

fn other(e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(anyhow::anyhow!("{e}"))
}

/// Compress one chunk, optionally with a trained dictionary.
pub fn compress(chunk: &[u8], dict: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut c = match dict {
        Some(d) => zstd::bulk::Compressor::with_dictionary(LEVEL, d).map_err(other)?,
        None => zstd::bulk::Compressor::new(LEVEL).map_err(other)?,
    };
    c.compress(chunk).map_err(other)
}

/// Decompress one chunk back to exactly `ulen` bytes.
pub fn decompress(data: &[u8], ulen: usize, dict: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut d = match dict {
        Some(d) => zstd::bulk::Decompressor::with_dictionary(d).map_err(other)?,
        None => zstd::bulk::Decompressor::new().map_err(other)?,
    };
    let out = d.decompress(data, ulen).map_err(other)?;
    if out.len() != ulen {
        return Err(other(format!(
            "chunk decompressed to {} bytes, expected {ulen}",
            out.len()
        )));
    }
    Ok(out)
}

/// Train a dictionary from sample buffers; `None` when there is not enough
/// material or the trainer fails (both are normal — compression then runs
/// without a dictionary).
pub fn train_dict(samples: &[Vec<u8>]) -> Option<Vec<u8>> {
    if samples.len() < DICT_MIN_SAMPLES {
        return None;
    }
    zstd::dict::from_samples(samples, DICT_MAX)
        .ok()
        .filter(|d| !d.is_empty())
}

/// Read one chunk out of an open pack file.
pub fn read_chunk(pack: &mut (impl Read + Seek), off: u64, clen: usize) -> Result<Vec<u8>> {
    pack.seek(SeekFrom::Start(off)).map_err(other)?;
    let mut buf = vec![0u8; clen];
    pack.read_exact(&mut buf).map_err(other)?;
    Ok(buf)
}

/// Check a pack file starts with the magic.
pub fn check_magic(pack: &mut (impl Read + Seek)) -> Result<()> {
    pack.seek(SeekFrom::Start(0)).map_err(other)?;
    let mut m = [0u8; 8];
    pack.read_exact(&mut m).map_err(other)?;
    if &m != PACK_MAGIC {
        return Err(other("not a FileMind pack file"));
    }
    Ok(())
}

/// A member's location marker as stored in `files.location`.
pub fn location(archive_id: &str, rel: &str) -> String {
    format!("archive:{archive_id}#{rel}")
}

/// Parse a `files.location` marker back into (archive_id, rel).
pub fn parse_location(loc: &str) -> Option<(&str, &str)> {
    let rest = loc.strip_prefix("archive:")?;
    rest.split_once('#')
}

/// File name for an archive's pack inside the archive folder.
pub fn pack_name(name: &str, archive_id: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if c == '/' || c == ':' { '-' } else { c })
        .collect();
    format!("{safe} ({archive_id}).fmpack")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: usize, tag: &str) -> Vec<u8> {
        format!("{tag}: a line of source code that repeats with variation\n")
            .repeat(n)
            .into_bytes()
    }

    /// Deterministic varied text — uniform repetition gives FastCDC no
    /// content to resynchronise on.
    fn varied(lines: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        let mut out = Vec::new();
        for i in 0..lines {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(format!("line {i} token {x:016x} of the corpus\n").as_bytes());
        }
        out
    }

    #[test]
    fn chunks_cover_and_dedupe() {
        let a = varied(30_000, 7);
        let spans = chunk_spans(&a);
        assert_eq!(spans.iter().map(|(_, l)| l).sum::<usize>(), a.len());
        assert!(spans.iter().all(|(_, l)| *l <= CHUNK_MAX as usize));
        // the same content chunked twice gives the same spans
        assert_eq!(spans, chunk_spans(&a));
        // a copy with an insertion up front still shares most chunk content
        let mut b = varied(50, 99);
        b.extend_from_slice(&a);
        let ha: std::collections::HashSet<blake3::Hash> = spans
            .iter()
            .map(|&(o, l)| blake3::hash(&a[o..o + l]))
            .collect();
        let shared = chunk_spans(&b)
            .iter()
            .filter(|&&(o, l)| ha.contains(&blake3::hash(&b[o..o + l])))
            .count();
        assert!(shared > spans.len() / 2, "{shared} of {}", spans.len());
    }

    #[test]
    fn compress_round_trips_with_and_without_dict() {
        let data = text(3_000, "body");
        let c = compress(&data, None).unwrap();
        assert!(c.len() < data.len() / 4);
        assert_eq!(decompress(&c, data.len(), None).unwrap(), data);

        let samples: Vec<Vec<u8>> = (0..16).map(|i| text(60, &format!("sample {i}"))).collect();
        let dict = train_dict(&samples).expect("enough samples");
        let small = text(4, "tiny");
        let cd = compress(&small, Some(&dict)).unwrap();
        let cn = compress(&small, None).unwrap();
        assert_eq!(decompress(&cd, small.len(), Some(&dict)).unwrap(), small);
        // the dictionary should help on tiny inputs that match its shape
        assert!(
            cd.len() <= cn.len() + 16,
            "dict {} plain {}",
            cd.len(),
            cn.len()
        );
        // wrong dictionary must fail loudly, not return wrong bytes
        assert!(decompress(&cd, small.len(), None).is_err());
    }

    #[test]
    fn train_dict_needs_samples() {
        assert!(train_dict(&[b"one".to_vec()]).is_none());
    }

    #[test]
    fn location_round_trips() {
        let l = location("arc_1", "src/main file.rs");
        assert_eq!(parse_location(&l), Some(("arc_1", "src/main file.rs")));
        assert_eq!(parse_location("nope"), None);
    }
}
