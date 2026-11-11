//! SHA-256 content fingerprints, stored as lowercase hex.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
}
