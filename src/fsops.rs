//! Conservative filesystem primitives. Nothing here follows the final
//! path component of a destination, and every write is atomic.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::OFlag;
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::hash::Hash;
use crate::paths::RelPath;

/// Explicit metadata a destination must have. `None` fields are not managed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl FileMeta {
    pub fn matches(&self, mode: u32, uid: u32, gid: u32) -> bool {
        self.mode.is_none_or(|m| m == mode & 0o7777)
            && self.uid.is_none_or(|u| u == uid)
            && self.gid.is_none_or(|g| g == gid)
    }
}

/// What is at a path right now, seen without following a final symlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    Missing,
    /// A symlink and its stored text (it may dangle).
    Symlink(PathBuf),
    File {
        hash: Hash,
        mode: u32,
        uid: u32,
        gid: u32,
    },
    Dir,
    /// FIFO, socket or device.
    Other,
}

pub fn inspect(path: &Path) -> Result<Observed> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Observed::Missing),
        Err(e) => return Err(e).ctx(|| format!("inspect {}", path.display())),
    };
    let kind = meta.file_type();
    if kind.is_symlink() {
        let text = fs::read_link(path).ctx(|| format!("read symlink {}", path.display()))?;
        return Ok(Observed::Symlink(text));
    }
    if kind.is_dir() {
        return Ok(Observed::Dir);
    }
    if !kind.is_file() {
        return Ok(Observed::Other);
    }
    let (mut file, meta) = open_regular(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .ctx(|| format!("read {}", path.display()))?;
    Ok(Observed::File {
        hash: Hash::of(&bytes),
        mode: meta.mode() & 0o7777,
        uid: meta.uid(),
        gid: meta.gid(),
    })
}

/// Opens a regular file for reading; fails on a symlink or special file.
fn open_regular(path: &Path) -> Result<(File, fs::Metadata)> {
    let flags = (OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .ctx(|| format!("open {} (symlinks are not followed)", path.display()))?;
    let meta = file.metadata().ctx(|| format!("stat {}", path.display()))?;
    if !meta.is_file() {
        return Err(Error::Failed(format!("{} is not a regular file", path.display())));
    }
    Ok((file, meta))
}

/// Reads a regular file without following a symlink at `path`.
pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    let (mut file, meta) = open_regular(path)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes)
        .ctx(|| format!("read {}", path.display()))?;
    Ok(bytes)
}

/// Replaces `dest` with `bytes` through a same-directory temporary file
/// that is private (0600) from creation, then gets owner and `mode`, and
/// is renamed over `dest`. A symlink at `dest` is replaced, not followed.
pub fn write_atomic(dest: &Path, bytes: &[u8], mode: u32, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    let parent = parent_of(dest)?;
    let what = || format!("write {}", dest.display());
    let mut tmp = tempfile::Builder::new()
        .prefix(".rustow-")
        .tempfile_in(parent)
        .ctx(what)?;
    tmp.write_all(bytes).ctx(what)?;
    if uid.is_some() || gid.is_some() {
        std::os::unix::fs::fchown(tmp.as_file(), uid, gid).ctx(|| format!("set owner of {}", dest.display()))?;
    }
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .ctx(what)?;
    tmp.as_file().sync_all().ctx(what)?;
    tmp.persist(dest)
        .map_err(|e| e.error)
        .ctx(|| format!("replace {}", dest.display()))?;
    sync_dir(parent);
    Ok(())
}

/// Creates or replaces the symlink `link` (never a directory) atomically.
pub fn symlink_atomic(link: &Path, target: &Path) -> Result<()> {
    let parent = parent_of(link)?;
    let what = || format!("create symlink {}", link.display());
    let tmp = tempfile::Builder::new()
        .prefix(".rustow-")
        .make_in(parent, |p| std::os::unix::fs::symlink(target, p))
        .ctx(what)?;
    tmp.persist(link).map_err(|e| e.error).ctx(what)?;
    sync_dir(parent);
    Ok(())
}

/// Changes owner and mode of an existing regular file, refusing symlinks.
pub fn set_meta(path: &Path, meta: FileMeta) -> Result<()> {
    let (file, _) = open_regular(path)?;
    if meta.uid.is_some() || meta.gid.is_some() {
        std::os::unix::fs::fchown(&file, meta.uid, meta.gid).ctx(|| format!("set owner of {}", path.display()))?;
    }
    if let Some(mode) = meta.mode {
        file.set_permissions(fs::Permissions::from_mode(mode))
            .ctx(|| format!("set mode of {}", path.display()))?;
    }
    Ok(())
}

/// Moves `path` (file or symlink) to `backup`, whose parent is created private.
pub fn backup(path: &Path, backup: &Path) -> Result<()> {
    ensure_private_dir(parent_of(backup)?)?;
    if fs::symlink_metadata(backup).is_ok() {
        return Err(Error::Failed(format!("backup {} already exists", backup.display())));
    }
    let what = || format!("back up {} to {}", path.display(), backup.display());
    match fs::rename(path, backup) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            if fs::symlink_metadata(path).ctx(what)?.file_type().is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(path).ctx(what)?, backup).ctx(what)?;
            } else {
                fs::copy(path, backup).ctx(what)?;
            }
            fs::remove_file(path).ctx(what)
        }
        other => other.ctx(what),
    }
}

/// Creates `dir` and missing parents with mode 0700, and checks that the
/// final directory is a real directory owned by the current user with no
/// group or other access (tightening it if needed).
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .ctx(|| format!("create {}", dir.display()))?;
    let meta = fs::symlink_metadata(dir).ctx(|| format!("inspect {}", dir.display()))?;
    if !meta.is_dir() {
        return Err(Error::Failed(format!(
            "{} is not a directory (symlinks are not followed)",
            dir.display()
        )));
    }
    let euid = geteuid().as_raw();
    if meta.uid() != euid {
        return Err(Error::Failed(format!(
            "{} is owned by uid {}, not by the current user (uid {euid})",
            dir.display(),
            meta.uid()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ctx(|| format!("restrict {}", dir.display()))?;
    }
    Ok(())
}

fn parent_of(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| Error::Failed(format!("{} has no parent directory", path.display())))
}

/// Best effort: makes a rename durable. Failure only weakens crash safety.
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// State of the directories above a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentState {
    /// The canonical parent directory exists.
    Ready(PathBuf),
    /// `existing` (canonical) exists; `missing` components must be created below it.
    Missing { existing: PathBuf, missing: Vec<String> },
    /// An ancestor is a file, a dangling or unsafe symlink, or not readable.
    Blocked { path: PathBuf, reason: String },
}

/// Walks `dest`'s ancestors below `scope_root` (canonical). Symlinked
/// ancestors are followed only to a directory owned by the current user
/// or root that others cannot write (sticky root-owned dirs are fine).
pub fn parent_state(scope_root: &Path, dest: &RelPath) -> Result<ParentState> {
    let Some(parent) = dest.parent() else {
        return Ok(ParentState::Ready(scope_root.to_path_buf()));
    };
    let parts: Vec<&str> = parent.components().collect();
    let mut cur = scope_root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        let next = cur.join(part);
        let blocked = |reason: String| {
            Ok(ParentState::Blocked {
                path: next.clone(),
                reason,
            })
        };
        match fs::symlink_metadata(&next) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let missing = parts[i..].iter().map(|s| (*s).to_owned()).collect();
                return Ok(ParentState::Missing { existing: cur, missing });
            }
            Err(e) => return blocked(e.to_string()),
            Ok(m) if m.is_dir() => cur = next,
            Ok(m) if m.file_type().is_symlink() => match safe_dir_target(&next) {
                Ok(real) => cur = real,
                Err(reason) => return blocked(reason),
            },
            Ok(_) => return blocked("not a directory".into()),
        }
    }
    Ok(ParentState::Ready(cur))
}

/// Resolves a symlinked ancestor to a directory owned by the current user
/// or root that others cannot write (unless sticky and root-owned).
fn safe_dir_target(link: &Path) -> std::result::Result<PathBuf, String> {
    let real = fs::canonicalize(link).map_err(|e| format!("symlink does not resolve: {e}"))?;
    let m = fs::metadata(&real).map_err(|e| e.to_string())?;
    if !m.is_dir() {
        return Err(format!(
            "symlink points to {}, which is not a directory",
            real.display()
        ));
    }
    let trusted_owner = m.uid() == geteuid().as_raw() || m.uid() == 0;
    let others_write = m.mode() & 0o022 != 0;
    let sticky_root = m.mode() & 0o1000 != 0 && m.uid() == 0;
    if !trusted_owner || (others_write && !sticky_root) {
        return Err(format!(
            "symlink points to {}, which is owned by another user or writable by others",
            real.display()
        ));
    }
    Ok(real)
}

/// Creates `missing` below `existing` with `mode` (subject to umask) and
/// returns the directories it created.
pub fn create_dirs(existing: &Path, missing: &[String], mode: u32) -> Result<Vec<PathBuf>> {
    let mut made = Vec::new();
    let mut cur = existing.to_path_buf();
    for part in missing {
        cur.push(part);
        match fs::DirBuilder::new().mode(mode).create(&cur) {
            Ok(()) => made.push(cur.clone()),
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(&cur).is_ok_and(|m| m.is_dir()) => {
            }
            Err(e) => return Err(e).ctx(|| format!("create directory {}", cur.display())),
        }
    }
    Ok(made)
}

/// UTC time as `YYYYmmddTHHMMSSZ`.
pub fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Owner and permission bits of one directory on a path.
#[derive(Debug, Clone)]
pub struct Owned {
    pub path: PathBuf,
    pub uid: u32,
    pub mode: u32,
}

/// Trust rule for a cache entry that a system symlink will point to:
/// every directory on its path is owned by root or by `owner` (the owner
/// of the destination directory) and is not writable by group or others
/// (sticky root-owned directories such as /tmp excepted). Directories
/// still to be created by `creator` must be created by a trusted user.
pub fn check_chain(chain: &[Owned], creator: Option<u32>, owner: u32) -> std::result::Result<(), String> {
    for dir in chain {
        if dir.uid != 0 && dir.uid != owner {
            return Err(format!(
                "{} is owned by uid {}, but the destination directory belongs to uid {owner}",
                dir.path.display(),
                dir.uid
            ));
        }
        let sticky_root = dir.mode & 0o1000 != 0 && dir.uid == 0;
        if dir.mode & 0o022 != 0 && !sticky_root {
            return Err(format!("{} is writable by group or others", dir.path.display()));
        }
    }
    match creator {
        Some(uid) if uid != 0 && uid != owner => Err(format!(
            "cache directories would be created by uid {uid}, not by uid {owner} or root"
        )),
        _ => Ok(()),
    }
}

/// Applies `check_chain` to the real path of a cache entry, which may not
/// exist yet (then the current user would create the missing part).
pub fn trust_cache(entry: &Path, owner: u32) -> std::result::Result<(), String> {
    let mut existing = entry;
    while fs::symlink_metadata(existing).is_err() {
        existing = existing.parent().ok_or("cache path has no existing ancestor")?;
    }
    let creator = (existing != entry).then(|| geteuid().as_raw());
    let real = fs::canonicalize(existing).map_err(|e| format!("{}: {e}", existing.display()))?;
    let mut chain = Vec::new();
    for dir in real.ancestors() {
        let m = fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        chain.push(Owned {
            path: dir.to_path_buf(),
            uid: m.uid(),
            mode: m.mode() & 0o7777,
        });
    }
    check_chain(&chain, creator, owner)
}

/// Owner of the deepest existing directory at or above `dir`.
pub fn owner_of(dir: &Path) -> Result<u32> {
    fs::metadata(dir)
        .map(|m| m.uid())
        .ctx(|| format!("inspect {}", dir.display()))
}

/// First executable file called `name` in `PATH`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().canonicalize().unwrap();
        (t, p)
    }

    fn mode_of(p: &Path) -> u32 {
        fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
    }

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    #[test]
    fn file_meta_matches_only_set_fields() {
        let m = FileMeta {
            mode: Some(0o600),
            uid: None,
            gid: Some(5),
        };
        assert!(m.matches(0o600, 1, 5));
        assert!(!m.matches(0o644, 1, 5));
        assert!(!m.matches(0o600, 1, 6));
        assert!(FileMeta::default().matches(0o777, 0, 0));
    }

    #[test]
    fn inspect_reports_each_kind() {
        let (_t, d) = tmp();
        assert_eq!(inspect(&d.join("none")).unwrap(), Observed::Missing);
        fs::write(d.join("f"), b"abc").unwrap();
        fs::set_permissions(d.join("f"), fs::Permissions::from_mode(0o640)).unwrap();
        match inspect(&d.join("f")).unwrap() {
            Observed::File { hash, mode, uid, .. } => {
                assert_eq!(hash, Hash::of(b"abc"));
                assert_eq!(mode, 0o640);
                assert_eq!(uid, nix::unistd::geteuid().as_raw());
            }
            other => panic!("{other:?}"),
        }
        symlink("nowhere", d.join("l")).unwrap();
        assert_eq!(inspect(&d.join("l")).unwrap(), Observed::Symlink("nowhere".into()));
        assert_eq!(inspect(&d).unwrap(), Observed::Dir);
        nix::unistd::mkfifo(&d.join("fifo"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        assert_eq!(inspect(&d.join("fifo")).unwrap(), Observed::Other);
    }

    #[test]
    fn read_file_refuses_symlink() {
        let (_t, d) = tmp();
        fs::write(d.join("f"), b"x").unwrap();
        symlink(d.join("f"), d.join("l")).unwrap();
        assert_eq!(read_file(&d.join("f")).unwrap(), b"x");
        assert!(read_file(&d.join("l")).is_err());
    }

    #[test]
    fn write_atomic_sets_mode_and_leaves_no_temp() {
        let (_t, d) = tmp();
        let f = d.join("out");
        write_atomic(&f, b"secret", 0o600, None, None).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"secret");
        assert_eq!(mode_of(&f), 0o600);
        write_atomic(&f, b"next", 0o644, None, None).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"next");
        assert_eq!(mode_of(&f), 0o644);
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
    }

    #[test]
    fn write_atomic_never_follows_dest_symlink() {
        let (_t, d) = tmp();
        fs::write(d.join("victim"), b"keep").unwrap();
        symlink(d.join("victim"), d.join("dest")).unwrap();
        write_atomic(&d.join("dest"), b"new", 0o644, None, None).unwrap();
        assert_eq!(fs::read(d.join("victim")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(d.join("dest")).unwrap().is_file());
    }

    #[test]
    fn write_atomic_refuses_directory() {
        let (_t, d) = tmp();
        fs::create_dir(d.join("dir")).unwrap();
        assert!(write_atomic(&d.join("dir"), b"x", 0o644, None, None).is_err());
        assert!(d.join("dir").is_dir());
    }

    #[test]
    fn symlink_atomic_replaces_symlink_but_not_directory() {
        let (_t, d) = tmp();
        symlink_atomic(&d.join("l"), Path::new("a")).unwrap();
        symlink_atomic(&d.join("l"), Path::new("b")).unwrap();
        assert_eq!(fs::read_link(d.join("l")).unwrap(), PathBuf::from("b"));
        fs::create_dir(d.join("dir")).unwrap();
        assert!(symlink_atomic(&d.join("dir"), Path::new("b")).is_err());
        assert!(d.join("dir").is_dir());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2);
    }

    #[test]
    fn set_meta_changes_mode_and_refuses_symlink() {
        let (_t, d) = tmp();
        fs::write(d.join("f"), b"x").unwrap();
        set_meta(
            &d.join("f"),
            FileMeta {
                mode: Some(0o600),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(mode_of(&d.join("f")), 0o600);
        symlink(d.join("f"), d.join("l")).unwrap();
        assert!(
            set_meta(
                &d.join("l"),
                FileMeta {
                    mode: Some(0o644),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(mode_of(&d.join("f")), 0o600);
    }

    #[test]
    fn backup_moves_into_private_dir() {
        let (_t, d) = tmp();
        fs::write(d.join("f"), b"data").unwrap();
        symlink("t", d.join("l")).unwrap();
        backup(&d.join("f"), &d.join("bk/a/f")).unwrap();
        backup(&d.join("l"), &d.join("bk/a/l")).unwrap();
        assert!(!d.join("f").exists());
        assert_eq!(fs::read(d.join("bk/a/f")).unwrap(), b"data");
        assert_eq!(fs::read_link(d.join("bk/a/l")).unwrap(), PathBuf::from("t"));
        assert_eq!(mode_of(&d.join("bk")), 0o700);
        assert_eq!(mode_of(&d.join("bk/a")), 0o700);
        // Never overwrite an existing backup.
        fs::write(d.join("f"), b"again").unwrap();
        assert!(backup(&d.join("f"), &d.join("bk/a/f")).is_err());
    }

    #[test]
    fn ensure_private_dir_creates_and_tightens() {
        let (_t, d) = tmp();
        ensure_private_dir(&d.join("a/b")).unwrap();
        assert_eq!(mode_of(&d.join("a/b")), 0o700);
        fs::set_permissions(d.join("a/b"), fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&d.join("a/b")).unwrap();
        assert_eq!(mode_of(&d.join("a/b")), 0o700);
        fs::create_dir(d.join("real")).unwrap();
        symlink(d.join("real"), d.join("link")).unwrap();
        assert!(ensure_private_dir(&d.join("link")).is_err());
        fs::write(d.join("file"), b"").unwrap();
        assert!(ensure_private_dir(&d.join("file")).is_err());
    }

    #[test]
    fn parent_state_cases() {
        let (_t, d) = tmp();
        fs::create_dir_all(d.join("h/.config")).unwrap();
        assert_eq!(
            parent_state(&d.join("h"), &rel(".bashrc")).unwrap(),
            ParentState::Ready(d.join("h"))
        );
        assert_eq!(
            parent_state(&d.join("h"), &rel(".config/nvim/lua/init.lua")).unwrap(),
            ParentState::Missing {
                existing: d.join("h/.config"),
                missing: vec!["nvim".into(), "lua".into()]
            }
        );
        // A safe symlinked ancestor is followed.
        fs::create_dir_all(d.join("data/cfg")).unwrap();
        symlink(d.join("data"), d.join("h/.local")).unwrap();
        assert_eq!(
            parent_state(&d.join("h"), &rel(".local/cfg/x")).unwrap(),
            ParentState::Ready(d.join("data/cfg"))
        );
        // A file, a dangling link, and a world-writable target block.
        fs::write(d.join("h/file"), b"").unwrap();
        assert!(matches!(
            parent_state(&d.join("h"), &rel("file/x")).unwrap(),
            ParentState::Blocked { .. }
        ));
        symlink(d.join("gone"), d.join("h/dangling")).unwrap();
        assert!(matches!(
            parent_state(&d.join("h"), &rel("dangling/x")).unwrap(),
            ParentState::Blocked { .. }
        ));
        fs::create_dir(d.join("open")).unwrap();
        fs::set_permissions(d.join("open"), fs::Permissions::from_mode(0o777)).unwrap();
        symlink(d.join("open"), d.join("h/open")).unwrap();
        assert!(matches!(
            parent_state(&d.join("h"), &rel("open/x")).unwrap(),
            ParentState::Blocked { .. }
        ));
    }

    #[test]
    fn create_dirs_reports_created() {
        let (_t, d) = tmp();
        let made = create_dirs(&d, &["a".into(), "b".into()], 0o700).unwrap();
        assert_eq!(made, vec![d.join("a"), d.join("a/b")]);
        assert_eq!(mode_of(&d.join("a/b")), 0o700);
    }

    fn owned(path: &str, uid: u32, mode: u32) -> Owned {
        Owned {
            path: path.into(),
            uid,
            mode,
        }
    }

    #[test]
    fn chain_trusted_when_owned_by_root_or_owner() {
        let chain = [
            owned("/", 0, 0o755),
            owned("/var", 0, 0o755),
            owned("/var/cache/rustow", 0, 0o700),
        ];
        assert!(check_chain(&chain, None, 0).is_ok());
        let user = [
            owned("/", 0, 0o755),
            owned("/home/u", 1000, 0o700),
            owned("/home/u/dot", 1000, 0o755),
        ];
        assert!(check_chain(&user, Some(1000), 1000).is_ok());
    }

    #[test]
    fn chain_rejects_foreign_owner() {
        let chain = [owned("/", 0, 0o755), owned("/home/u", 1000, 0o700)];
        let e = check_chain(&chain, None, 0).unwrap_err();
        assert!(e.contains("/home/u") && e.contains("1000"), "{e}");
    }

    #[test]
    fn chain_rejects_writable_by_others_but_allows_sticky_root() {
        let open = [owned("/", 0, 0o755), owned("/shared", 0, 0o775)];
        assert!(check_chain(&open, None, 0).unwrap_err().contains("writable"));
        let tmp = [
            owned("/", 0, 0o755),
            owned("/tmp", 0, 0o1777),
            owned("/tmp/x", 1000, 0o700),
        ];
        assert!(check_chain(&tmp, None, 1000).is_ok());
        let sticky_user = [owned("/", 0, 0o755), owned("/tmp", 1000, 0o1777)];
        assert!(check_chain(&sticky_user, None, 1000).is_err());
    }

    #[test]
    fn chain_rejects_untrusted_creator() {
        let chain = [owned("/", 0, 0o755)];
        assert!(check_chain(&chain, Some(1000), 0).unwrap_err().contains("1000"));
        assert!(check_chain(&chain, Some(0), 0).is_ok());
    }

    #[test]
    fn timestamp_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 16, "{t}");
        assert!(
            t.starts_with("20") && t.ends_with('Z') && t.as_bytes()[8] == b'T',
            "{t}"
        );
    }

    #[test]
    fn civil_date_known_values() {
        assert_eq!(format_utc(0), "19700101T000000Z");
        assert_eq!(format_utc(1_762_905_600), "20251112T000000Z");
        assert_eq!(format_utc(951_782_400 + 3_661), "20000229T010101Z");
    }
}
