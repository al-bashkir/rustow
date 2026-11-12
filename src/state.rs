//! Persistent deployment state: one private JSON file per repository,
//! with entries keyed by absolute destination path. State is a hint for
//! ownership; the planner always inspects the real filesystem too.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Backend;
use crate::error::{Error, IoContext, Result};
use crate::fsops::{self, FileMeta};
use crate::hash::Hash;
use crate::paths::{RelPath, Scope};

pub const STATE_VERSION: u32 = 1;

/// What rustow last applied at one destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub source: RelPath,
    pub scope: Scope,
    /// `Mode::label()` of the applied mode.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<Backend>,
    /// Hash of the source file bytes (ciphertext for encrypted sources).
    pub source_hash: Hash,
    /// Content hash of the copied destination, or of the cache entry
    /// behind a cache-backed symlink. Sensitive: protect like the content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_hash: Option<Hash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_id: Option<Hash>,
    /// Text of the symlink rustow created at the destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<FileMeta>,
    /// The destination is a folded directory symlink.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded_dir: bool,
}

/// Last ciphertext rustow wrote for an encrypted source, and to whom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncRecord {
    pub ciphertext_hash: Hash,
    pub recipients_id: Hash,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub repo: PathBuf,
    #[serde(default)]
    pub entries: BTreeMap<PathBuf, Record>,
    /// Directories rustow created; removed again only when empty.
    #[serde(default)]
    pub created_dirs: BTreeSet<PathBuf>,
    #[serde(default)]
    pub encryption: BTreeMap<RelPath, EncRecord>,
    /// Sources that `rustow file add` registered for copy deployment on this machine.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub files: BTreeSet<RelPath>,
    /// Sources that `rustow link <path>` registered in copy mode.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub links: BTreeSet<RelPath>,
}

impl State {
    pub fn file(dir: &Path, repo_id: &str) -> PathBuf {
        dir.join(format!("{repo_id}.json"))
    }

    /// Loads the state of `repo`; a missing file gives an empty state.
    pub fn load(dir: &Path, repo_id: &str, repo: &Path) -> Result<State> {
        let file = State::file(dir, repo_id);
        let bytes = match fsops::read_file(&file) {
            Ok(b) => b,
            Err(_) if fs::symlink_metadata(&file).is_err() => {
                return Ok(State {
                    version: STATE_VERSION,
                    repo: repo.to_path_buf(),
                    ..Default::default()
                });
            }
            Err(e) => return Err(e),
        };
        let bad = |why: String| {
            Error::Failed(format!(
                "state file {}: {why}; move it away to start over",
                file.display()
            ))
        };
        let state: State = serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?;
        if state.version != STATE_VERSION {
            return Err(bad(format!("unsupported state version {}", state.version)));
        }
        if state.repo != repo {
            return Err(bad(format!("it belongs to repository {}", state.repo.display())));
        }
        Ok(state)
    }

    /// Writes the state privately and atomically; skips identical content.
    pub fn save(&self, dir: &Path, repo_id: &str) -> Result<()> {
        fsops::ensure_private_dir(dir)?;
        let file = State::file(dir, repo_id);
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|e| Error::Failed(format!("encode state: {e}")))?;
        bytes.push(b'\n');
        if fsops::read_file(&file).is_ok_and(|old| old == bytes) {
            return Ok(());
        }
        fsops::write_atomic(&file, &bytes, 0o600, None, None)
    }
}

/// Exclusive per-repository lock, held while a command changes anything.
pub struct Lock {
    _file: File,
}

pub fn lock(dir: &Path, repo_id: &str) -> Result<Lock> {
    fsops::ensure_private_dir(dir)?;
    let path = dir.join(format!("{repo_id}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(&path)
        .ctx(|| format!("open lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Lock { _file: file }),
        Err(fs::TryLockError::WouldBlock) => Err(Error::Failed(format!(
            "another rustow process is using this repository (lock {})",
            path.display()
        ))),
        Err(fs::TryLockError::Error(e)) => Err(e).ctx(|| format!("lock {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sample() -> State {
        let mut s = State {
            version: STATE_VERSION,
            repo: "/r".into(),
            ..Default::default()
        };
        s.entries.insert(
            "/h/.bashrc".into(),
            Record {
                source: RelPath::parse(".bashrc").unwrap(),
                scope: Scope::Home,
                mode: "link".into(),
                backend: None,
                source_hash: Hash::of(b"x"),
                applied_hash: None,
                render_id: None,
                link_target: Some("dot/.bashrc".into()),
                cache_path: None,
                meta: None,
                folded_dir: false,
            },
        );
        s.created_dirs.insert("/h/.config".into());
        s.encryption.insert(
            RelPath::parse("a.age").unwrap(),
            EncRecord {
                ciphertext_hash: Hash::of(b"c"),
                recipients_id: Hash::of(b"r"),
            },
        );
        s
    }

    #[test]
    fn missing_state_is_empty() {
        let d = tempfile::tempdir().unwrap();
        let s = State::load(&d.path().join("st"), "id", Path::new("/r")).unwrap();
        assert!(s.entries.is_empty());
        assert_eq!(s.repo, PathBuf::from("/r"));
        assert_eq!(s.version, STATE_VERSION);
    }

    #[test]
    fn state_roundtrip_with_private_modes() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("st");
        let s = sample();
        s.save(&dir, "id").unwrap();
        assert_eq!(State::load(&dir, "id", Path::new("/r")).unwrap(), s);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&State::file(&dir, "id")), 0o600);
    }

    #[test]
    fn save_skips_identical_content() {
        let d = tempfile::tempdir().unwrap();
        let s = sample();
        s.save(d.path(), "id").unwrap();
        let file = State::file(d.path(), "id");
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        s.save(d.path(), "id").unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), before);
    }

    #[test]
    fn corrupt_or_foreign_state_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(State::file(d.path(), "id"), "{not json").unwrap();
        let e = State::load(d.path(), "id", Path::new("/r")).unwrap_err().to_string();
        assert!(e.contains("id.json"), "{e}");
        let mut other = sample();
        other.repo = "/elsewhere".into();
        std::fs::remove_file(State::file(d.path(), "id")).unwrap();
        other.save(d.path(), "id").unwrap();
        assert!(State::load(d.path(), "id", Path::new("/r")).is_err());
        let mut future = sample();
        future.version = 99;
        std::fs::remove_file(State::file(d.path(), "id")).unwrap();
        future.save(d.path(), "id").unwrap();
        assert!(State::load(d.path(), "id", Path::new("/r")).is_err());
    }

    #[test]
    fn second_lock_fails_fast() {
        let d = tempfile::tempdir().unwrap();
        let first = lock(d.path(), "id").unwrap();
        let e = lock(d.path(), "id").err().unwrap().to_string();
        assert!(e.contains("another rustow"), "{e}");
        drop(first);
        assert!(lock(d.path(), "id").is_ok());
    }
}
