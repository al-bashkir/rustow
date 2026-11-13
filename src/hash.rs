//! SHA-256 content fingerprints, stored as lowercase hex.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// The buffer size of `Hash::copy`.
const CHUNK: usize = 64 << 10;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hash(String);

impl Hash {
    pub fn of(bytes: &[u8]) -> Hash {
        Hash::of_parts(&[bytes])
    }

    /// Hash of several byte strings, each prefixed by its length so that
    /// different splits of the same bytes never collide.
    pub fn of_parts(parts: &[&[u8]]) -> Hash {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        Hash::hex(hasher)
    }

    /// Copies exactly `len` bytes from `from` to `to` and returns their
    /// hash, the value that `Hash::of` gives. The bytes pass through one
    /// 64 KiB buffer that is zeroed afterwards, so the size of the content
    /// does not count. Input shorter than `len` is an error.
    pub fn copy(from: &mut dyn Read, len: u64, to: &mut dyn Write) -> io::Result<Hash> {
        let mut hasher = Sha256::new();
        hasher.update(len.to_le_bytes());
        // Most files are small: zeroing 64 KiB for each would cost more than the hash.
        let mut buf = Zeroizing::new(vec![0; len.min(CHUNK as u64) as usize]);
        let mut left = len;
        while left > 0 {
            let chunk = &mut buf[..left.min(CHUNK as u64) as usize];
            from.read_exact(chunk).map_err(|e| match e.kind() {
                io::ErrorKind::UnexpectedEof => io::Error::new(e.kind(), "the content ended early"),
                _ => e,
            })?;
            hasher.update(&*chunk);
            to.write_all(chunk)?;
            left -= chunk.len() as u64;
        }
        Ok(Hash::hex(hasher))
    }

    fn hex(hasher: Sha256) -> Hash {
        Hash(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// First 16 hex digits: enough to name cache and state directories.
    pub fn short(&self) -> &str {
        &self.0[..16]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_hex_and_split_sensitive() {
        let h = Hash::of(b"abc");
        assert_eq!(h.as_str().len(), 64);
        assert!(
            h.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(h, Hash::of(b"abc"));
        assert_ne!(h, Hash::of(b"abd"));
        assert_eq!(h.short().len(), 16);
        assert_ne!(Hash::of_parts(&[b"ab", b"c"]), Hash::of_parts(&[b"a", b"bc"]));
    }

    #[test]
    fn copy_streams_exactly_len_bytes_and_hashes_as_of() {
        let data: Vec<u8> = (0..CHUNK * 2 + 7).map(|i| i as u8).collect();
        let mut out = Vec::new();
        let h = Hash::copy(&mut data.as_slice(), data.len() as u64, &mut out).unwrap();
        assert_eq!((h, &out), (Hash::of(&data), &data));
        out.clear();
        let h = Hash::copy(&mut data.as_slice(), 3, &mut out).unwrap();
        assert_eq!((h, out.as_slice()), (Hash::of(&data[..3]), &data[..3]));
        let e = Hash::copy(&mut &data[..5], 6, &mut io::sink()).unwrap_err();
        assert_eq!(e.to_string(), "the content ended early");
    }
}
