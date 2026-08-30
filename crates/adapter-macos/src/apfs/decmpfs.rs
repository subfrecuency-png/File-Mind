//! The on-disk format of macOS transparent compression (`decmpfs`), zlib
//! flavour — what `ditto --hfsCompression` and `afsctool` write.
//!
//! A compressed file has an empty data fork, the `UF_COMPRESSED` flag and:
//!
//! * `com.apple.decmpfs` xattr: 16-byte header (little-endian: magic
//!   `fpmc`, compression type, uncompressed size). Type 3 keeps the zlib
//!   stream right after the header (whole xattr ≤ 3802 bytes); type 4 puts
//!   the data in the resource fork.
//! * `com.apple.ResourceFork` xattr (type 4): a classic resource fork
//!   (big-endian header at 0, data at 0x100, 50-byte map at the end) whose
//!   single `cmpf` resource is a little-endian block table (count, then
//!   offset/size pairs relative to the table) followed by one zlib stream
//!   per 64 KiB of input. A chunk that would not shrink is stored as `0xFF`
//!   followed by the raw bytes.
//!
//! Both directions are implemented so a payload can be checked before it
//! is activated, and so recovery never has to trust the kernel's view.

use std::io::{Read, Write};

pub const XATTR_NAME: &str = "com.apple.decmpfs";
pub const RSRC_NAME: &str = "com.apple.ResourceFork";
pub const MAGIC: u32 = 0x636d_7066; // "fpmc"
pub const TYPE_ZLIB_XATTR: u32 = 3;
pub const TYPE_ZLIB_RSRC: u32 = 4;
/// Largest `com.apple.decmpfs` xattr the kernel accepts.
pub const MAX_XATTR_BYTES: usize = 3802;
pub const CHUNK: usize = 64 * 1024;
const HEADER: usize = 16;
const RSRC_DATA_AT: usize = 0x100;

/// The two xattrs that make a compressed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub decmpfs: Vec<u8>,
    pub rsrc: Option<Vec<u8>>,
}

impl Encoded {
    /// Bytes the payload will occupy (xattr + resource fork).
    pub fn len(&self) -> usize {
        self.decmpfs.len() + self.rsrc.as_ref().map_or(0, Vec::len)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn header(kind: u32, size: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER);
    h.extend_from_slice(&MAGIC.to_le_bytes());
    h.extend_from_slice(&kind.to_le_bytes());
    h.extend_from_slice(&size.to_le_bytes());
    h
}

/// Parse a `com.apple.decmpfs` header: (compression type, uncompressed size).
pub fn parse_header(x: &[u8]) -> Option<(u32, u64)> {
    if x.len() < HEADER || u32::from_le_bytes(x[0..4].try_into().ok()?) != MAGIC {
        return None;
    }
    Some((
        u32::from_le_bytes(x[4..8].try_into().ok()?),
        u64::from_le_bytes(x[8..16].try_into().ok()?),
    ))
}

fn zlib(chunk: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(
        Vec::with_capacity(chunk.len() / 2 + 64),
        flate2::Compression::default(),
    );
    match e.write_all(chunk).and_then(|_| e.finish()) {
        Ok(v) if v.len() < chunk.len() => v,
        _ => {
            let mut raw = Vec::with_capacity(chunk.len() + 1);
            raw.push(0xFF);
            raw.extend_from_slice(chunk);
            raw
        }
    }
}

fn unzlib(c: &[u8], expect: usize) -> std::io::Result<Vec<u8>> {
    if c.first() == Some(&0xFF) {
        return Ok(c[1..].to_vec());
    }
    let mut out = Vec::with_capacity(expect);
    flate2::read::ZlibDecoder::new(c).read_to_end(&mut out)?;
    Ok(out)
}

/// The 50-byte resource map: one type (`cmpf`), one resource (id 1), no name.
const MAP: [u8; 50] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // reserved (copy of header)
    0, 0, 0, 0, // handle to next map
    0, 0, // file reference number
    0, 0, // fork attributes
    0, 0x1C, // offset to type list
    0, 0x32, // offset to name list
    0, 0, // number of types − 1
    b'c', b'm', b'p', b'f', // type
    0, 0, // number of resources of this type − 1
    0, 0x0A, // offset to reference list
    0, 1, // resource id
    0xFF, 0xFF, // no name
    0, 0, 0, 0, // attributes + data offset
    0, 0, 0, 0, // handle
];

/// Encode `data` the way the kernel expects. Small results go inline
/// (type 3); everything else into a resource fork (type 4).
pub fn encode(data: &[u8]) -> Encoded {
    let size = data.len() as u64;
    if data.len() <= CHUNK {
        let z = zlib(data);
        if HEADER + z.len() <= MAX_XATTR_BYTES {
            let mut x = header(TYPE_ZLIB_XATTR, size);
            x.extend_from_slice(&z);
            return Encoded {
                decmpfs: x,
                rsrc: None,
            };
        }
    }
    let chunks: Vec<Vec<u8>> = data.chunks(CHUNK).map(zlib).collect();
    let n = chunks.len();
    let mut table = Vec::with_capacity(4 + 8 * n);
    table.extend_from_slice(&(n as u32).to_le_bytes());
    let mut off = 4u32 + 8 * n as u32;
    for c in &chunks {
        table.extend_from_slice(&off.to_le_bytes());
        table.extend_from_slice(&(c.len() as u32).to_le_bytes());
        off += c.len() as u32;
    }
    let blob_len = table.len() + chunks.iter().map(Vec::len).sum::<usize>();
    let data_len = 4 + blob_len;
    let map_off = RSRC_DATA_AT + data_len;
    let mut r = Vec::with_capacity(map_off + MAP.len());
    r.extend_from_slice(&(RSRC_DATA_AT as u32).to_be_bytes());
    r.extend_from_slice(&(map_off as u32).to_be_bytes());
    r.extend_from_slice(&(data_len as u32).to_be_bytes());
    r.extend_from_slice(&(MAP.len() as u32).to_be_bytes());
    r.resize(RSRC_DATA_AT, 0);
    r.extend_from_slice(&(blob_len as u32).to_be_bytes());
    r.extend_from_slice(&table);
    for c in &chunks {
        r.extend_from_slice(c);
    }
    r.extend_from_slice(&MAP);
    Encoded {
        decmpfs: header(TYPE_ZLIB_RSRC, size),
        rsrc: Some(r),
    }
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("decmpfs: {msg}"))
}

fn be32(b: &[u8], at: usize) -> std::io::Result<usize> {
    Ok(u32::from_be_bytes(
        b.get(at..at + 4)
            .ok_or_else(|| bad("short resource fork"))?
            .try_into()
            .unwrap(),
    ) as usize)
}

fn le32(b: &[u8], at: usize) -> std::io::Result<usize> {
    Ok(u32::from_le_bytes(
        b.get(at..at + 4)
            .ok_or_else(|| bad("short block table"))?
            .try_into()
            .unwrap(),
    ) as usize)
}

/// Decode a payload back to the original bytes.
pub fn decode(decmpfs: &[u8], rsrc: Option<&[u8]>) -> std::io::Result<Vec<u8>> {
    let (kind, size) = parse_header(decmpfs).ok_or_else(|| bad("bad header"))?;
    let size = size as usize;
    match kind {
        TYPE_ZLIB_XATTR => {
            let out = unzlib(&decmpfs[HEADER..], size)?;
            if out.len() != size {
                return Err(bad("inline size mismatch"));
            }
            Ok(out)
        }
        TYPE_ZLIB_RSRC => {
            let r = rsrc.ok_or_else(|| bad("type 4 without a resource fork"))?;
            let data_at = be32(r, 0)?;
            let data_len = be32(r, 8)?;
            if data_at + data_len > r.len() {
                return Err(bad("data area past the end"));
            }
            let blob = &r[data_at + 4..data_at + data_len];
            let n = le32(blob, 0)?;
            let mut out = Vec::with_capacity(size);
            for i in 0..n {
                let off = le32(blob, 4 + 8 * i)?;
                let len = le32(blob, 8 + 8 * i)?;
                let c = blob
                    .get(off..off + len)
                    .ok_or_else(|| bad("chunk past the end"))?;
                let want = (size - out.len()).min(CHUNK);
                let d = unzlib(c, want)?;
                out.extend_from_slice(&d);
            }
            if out.len() != size {
                return Err(bad("size mismatch"));
            }
            Ok(out)
        }
        other => Err(bad(&format!("unsupported compression type {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) -> Encoded {
        let e = encode(data);
        let back = decode(&e.decmpfs, e.rsrc.as_deref()).unwrap();
        assert_eq!(back, data);
        assert_eq!(parse_header(&e.decmpfs).unwrap().1, data.len() as u64);
        e
    }

    #[test]
    fn small_text_goes_inline() {
        let e = roundtrip(&b"hello apfs compression\n".repeat(100));
        assert!(e.rsrc.is_none());
        assert_eq!(parse_header(&e.decmpfs).unwrap().0, TYPE_ZLIB_XATTR);
        assert!(e.decmpfs.len() <= MAX_XATTR_BYTES);
    }

    #[test]
    fn empty_file() {
        let e = roundtrip(b"");
        assert!(e.rsrc.is_none());
    }

    #[test]
    fn big_and_mixed_go_to_the_resource_fork() {
        let text: Vec<u8> = b"line of a log file with some repetition\n".repeat(30_000);
        let e = roundtrip(&text);
        let r = e.rsrc.as_ref().unwrap();
        assert_eq!(parse_header(&e.decmpfs).unwrap().0, TYPE_ZLIB_RSRC);
        assert!(e.len() < text.len() / 20, "{}", e.len());
        // header bookkeeping
        assert_eq!(&r[0..4], &0x100u32.to_be_bytes());
        let map_off = u32::from_be_bytes(r[4..8].try_into().unwrap()) as usize;
        assert_eq!(map_off + 50, r.len());
        assert_eq!(&r[map_off..], &MAP);

        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let noise: Vec<u8> = (0..200_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let mut mixed = text[..100_000].to_vec();
        mixed.extend_from_slice(&noise);
        mixed.extend_from_slice(&text[..70_000]);
        let e = roundtrip(&mixed);
        // noise chunks are stored raw (0xFF + bytes), so the total is a bit over the noise
        assert!(e.len() > 200_000 && e.len() < 210_000, "{}", e.len());
        roundtrip(&vec![b'x'; CHUNK + 1]);
        roundtrip(&vec![b'x'; CHUNK]);
        roundtrip(&noise[..5000]);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"nope", None).is_err());
        let e = encode(&b"abc".repeat(50_000));
        assert!(decode(&e.decmpfs, None).is_err());
        let mut r = e.rsrc.clone().unwrap();
        r.truncate(300);
        assert!(decode(&e.decmpfs, Some(&r)).is_err());
    }
}
