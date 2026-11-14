//! Conservative filesystem primitives. Nothing here follows the final
//! path component of a destination, and every write is atomic.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::OFlag;
use nix::unistd::geteuid;
use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::hash::Hash;
use crate::paths::RelPath;

mod dir;
pub use dir::{Dir, DirId, Walked, check_planned, journal_to, open_planned, open_ready, temp_name};

/// Name prefix of every temporary file rustow creates. Names with it are
/// reserved at any depth, so a file left by a crash is never deployed.
pub const TEMP_PREFIX: &str = ".rustow-tmp-";

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A regular file that the current user cannot read: only its
    /// metadata. The plan compares it with the state; the sudo helper
    /// reads it as root.
    Unreadable {
        mode: u32,
        uid: u32,
        gid: u32,
    },
    /// An entry of a directory that the current user cannot search:
    /// nothing is known about it. The plan compares it with the state; the
    /// sudo helper looks at it as root.
    Hidden,
    Dir,
    /// FIFO, socket or device.
    Other,
}

/// What is at `path` now (`Dir::inspect`). The directories above it are
/// resolved as the kernel does: this only reads.
pub fn inspect(path: &Path) -> Result<Observed> {
    let (parent, name) = split(path)?;
    match Dir::at(parent) {
        Ok(dir) => dir.inspect(name),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Observed::Missing),
        // Without `O_PATH` (macOS), a parent that the user may not search
        // does not open at all: as on Linux, its entry is hidden. Its `stat`
        // needs search only above it, so a denied directory above the
        // parent stays an error, as on Linux.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && fs::metadata(parent).is_ok_and(|m| m.is_dir()) => {
            Ok(Observed::Hidden)
        }
        Err(e) => Err(e).ctx(|| format!("inspect {}", path.display())),
    }
}

/// Opens a file for reading with `flags` and its metadata. `O_NONBLOCK`
/// keeps a FIFO from blocking the open; the caller checks the file type.
fn open_nonblocking(path: &Path, flags: OFlag) -> Result<(File, fs::Metadata)> {
    let note = match flags.contains(OFlag::O_NOFOLLOW) {
        true => " (symlinks are not followed)",
        false => "",
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((flags | OFlag::O_NONBLOCK).bits())
        .open(path)
        .ctx(|| format!("open {}{note}", path.display()))?;
    let meta = file.metadata().ctx(|| format!("stat {}", path.display()))?;
    Ok((file, meta))
}

/// Opens a regular file for reading; fails on a symlink or special file.
pub fn open_regular(path: &Path) -> Result<(File, fs::Metadata)> {
    let (file, meta) = open_nonblocking(path, OFlag::O_NOFOLLOW)?;
    if !meta.is_file() {
        return Err(Error::Failed(format!("{} is not a regular file", path.display())));
    }
    Ok((file, meta))
}

/// Reads an input file of rustow (configuration, ignore rules, state): a
/// regular file of at most `limit` bytes, opened with `flags`. A FIFO, a
/// device, a directory, a symlink under `O_NOFOLLOW` or a larger file is a
/// configuration error that names the file, never its content. So is a
/// symlink that does not resolve: it is not a missing file.
pub fn read_limited(path: &Path, flags: OFlag, limit: u64) -> Result<Vec<u8>> {
    let not_regular = || Error::Config(format!("{} is not a regular file", path.display()));
    let symlink = || fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink());
    let (file, meta) = match open_nonblocking(path, flags) {
        Err(_) if flags.contains(OFlag::O_NOFOLLOW) && symlink() => return Err(not_regular()),
        Err(e) if e.is_not_found() && symlink() => {
            return Err(Error::Config(format!(
                "{} is a symlink that does not resolve",
                path.display()
            )));
        }
        opened => opened?,
    };
    if !meta.is_file() {
        return Err(not_regular());
    }
    read_at_most(file, &path.display().to_string(), limit)
}

/// Reads at most `limit` bytes from `reader` (called `name` in messages); more is an error.
pub fn read_at_most(reader: impl Read, name: &str, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ctx(|| format!("read {name}"))?;
    if bytes.len() as u64 > limit {
        return Err(Error::Config(format!("{name} is larger than {} MiB", limit >> 20)));
    }
    Ok(bytes)
}

/// Reads a regular file without following a symlink at `path`.
pub fn read_file(path: &Path) -> Result<Vec<u8>> {
    let (mut file, meta) = open_regular(path)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut bytes)
        .ctx(|| format!("read {}", path.display()))?;
    Ok(bytes)
}

/// Hash and length of a regular file, without following a symlink at
/// `path`. The file is streamed (`Hash::copy`), so its size does not count.
pub fn hash_file(path: &Path) -> Result<(Hash, u64)> {
    let (mut file, meta) = open_regular(path)?;
    let hash = Hash::copy(&mut file, meta.len(), &mut io::sink()).ctx(|| format!("read {}", path.display()))?;
    Ok((hash, meta.len()))
}

/// Replaces `dest` with `bytes` (`Dir::write`) in the directory of `dest`,
/// walked for the current user. A symlink at `dest` is replaced, not followed.
pub fn write_atomic(dest: &Path, bytes: &[u8], mode: u32, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    write_path(dest, bytes, (mode, uid, gid), true)
}

/// Writes the new file `dest` (mode `mode`) as `write_atomic` does, but
/// never over anything there.
pub fn write_new(dest: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    write_path(dest, bytes, (mode, None, None), false)
}

fn write_path(
    dest: &Path,
    bytes: &[u8],
    (mode, uid, gid): (u32, Option<u32>, Option<u32>),
    replace: bool,
) -> Result<()> {
    let dest = std::path::absolute(dest).ctx(|| format!("write {}", dest.display()))?;
    let (parent, name) = split(&dest)?;
    let meta = FileMeta {
        mode: Some(mode),
        uid,
        gid,
    };
    let want = (&Hash::of(bytes), bytes.len() as u64);
    let dir = open_ready(parent, geteuid().as_raw())?;
    dir.write(name, &temp_name(&[parent])?, &mut &bytes[..], want, meta, replace)
}

/// Moves the file or symlink `name` of `dir` to `backup`, whose parent
/// must exist (the user made it: the sudo helper makes no directory) and
/// is walked for `me`; never over an existing backup. `temp` names the
/// copy across filesystems. A `private` backup is of a file that `me`
/// could not see: it keeps only its owner bits before it gets into the
/// directory of `me` (`Dir::restrict`), and its old mode when the move fails.
pub fn backup(dir: &Dir, name: &OsStr, backup: &Path, me: u32, temp: &str, private: bool) -> Result<()> {
    let (to, to_name) = split(backup)?;
    let to = open_ready(to, me)?;
    let restricted = match private {
        true => dir.restrict(name)?,
        false => None,
    };
    let moved = dir.move_to(name, &to, to_name, temp);
    if let (Err(_), Some((file, mode))) = (&moved, restricted) {
        // Best effort: the item fails anyway.
        let _ = file.set_permissions(fs::Permissions::from_mode(mode));
    }
    moved
}

/// Removes each of `dirs`, deepest first, while it is an empty directory,
/// and returns the removed ones. Each parent is walked again for `me`, so
/// no directory stays open per level.
pub fn remove_empty<'a>(dirs: impl IntoIterator<Item = &'a Path>, me: u32) -> Vec<PathBuf> {
    let gone = |dir: &Path| -> Result<bool> {
        let (parent, name) = split(dir)?;
        Ok(open_ready(parent, me)?.rmdir(name))
    };
    let mut removed = Vec::new();
    for dir in dirs {
        if !gone(dir).unwrap_or(false) {
            break;
        }
        removed.push(dir.to_path_buf());
    }
    removed
}

/// Removes `dir` and each empty directory below it, deepest first; never
/// follows a symlink below `dir`. A directory that holds anything stays.
pub fn remove_empty_tree(dir: &Path) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            remove_empty_tree(&entry.path());
        }
    }
    fs::remove_dir(dir).ok();
}

/// Creates `dir` and missing parents with mode 0700. The final directory
/// must be a real directory that the current user owns; group and other
/// access is removed (`Dir::private`). So is the setgid bit that a new
/// parent inherits.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    private_dir(dir).map(drop)
}

fn private_dir(dir: &Path) -> Result<Dir> {
    let (parent, name) = split(dir)?;
    let me = geteuid().as_raw();
    let base = (parent.ancestors().find(|a| fs::symlink_metadata(a).is_ok())).unwrap_or(parent);
    let mut cur = open_ready(base, me)?;
    for part in parent.strip_prefix(base).unwrap_or(Path::new("")).components() {
        cur = cur.private(part.as_os_str(), me)?;
    }
    cur.private(name, me)
}

/// The directory `dir` below the private directory `root`, open. Each
/// level from `root` down is a private directory (`Dir::private`) opened
/// from the one above it, so a symlink at any level is an error and
/// nothing written into `dir` can leave the tree.
pub fn private_tree(root: &Path, dir: &Path) -> Result<Dir> {
    let below = dir
        .strip_prefix(root)
        .map_err(|_| Error::Failed(format!("{} is not inside {}", dir.display(), root.display())))?;
    let me = geteuid().as_raw();
    let mut cur = private_dir(root)?;
    for part in below.components() {
        cur = cur.private(part.as_os_str(), me)?;
    }
    Ok(cur)
}

/// The parent directory and the last component of `path`.
pub fn split(path: &Path) -> Result<(&Path, &OsStr)> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok((parent, name)),
        _ => Err(Error::Failed(format!("{} has no parent directory", path.display()))),
    }
}

/// State of the directories above a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentState {
    /// The parent directory exists at the real path `dir`.
    Ready { dir: PathBuf, id: DirId },
    /// `existing` (real path) exists; `missing` components must be created below it.
    Missing {
        existing: PathBuf,
        id: DirId,
        missing: Vec<String>,
    },
    /// An ancestor is a file, a dangling or unsafe symlink, or not readable.
    Blocked { path: PathBuf, reason: String },
}

/// Walks `dest`'s ancestors below `scope_root` with `Dir::walk`: a
/// symlinked ancestor is followed only when `check_dir` accepts, for
/// `me`, the symlink's owner and the directory it reaches.
pub fn parent_state(scope_root: &Path, dest: &RelPath, me: u32) -> Result<ParentState> {
    let parent = dest
        .parent()
        .map_or_else(|| scope_root.to_path_buf(), |p| scope_root.join(p.as_path()));
    Ok(match Dir::open(&parent, me)? {
        Walked::Ready(dir) => ParentState::Ready {
            id: dir.id()?,
            dir: dir.path,
        },
        Walked::Missing { dir, missing } => ParentState::Missing {
            id: dir.id()?,
            existing: dir.path,
            missing,
        },
        Walked::Blocked { path, reason } => ParentState::Blocked { path, reason },
        Walked::Sealed { path, id } => ParentState::Ready { dir: path, id },
    })
}

/// UTC time as `YYYYmmddTHHMMSSZ`.
pub fn timestamp() -> String {
    // A clock before 1970 gives the epoch.
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    format_utc(since.unwrap_or_default().as_secs())
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

/// Owner, group and permission bits of one path on a link target's chain.
#[derive(Debug, Clone)]
pub struct Owned {
    pub path: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    /// An extended POSIX ACL is set: the group bits of `mode` are its mask.
    pub acl: bool,
}

/// Trust rule for the file a symlink will point to: every path on the
/// chain is owned by root or by `owner` (the owner of the destination
/// directory), is not writable by others, and is writable by its group
/// only when that group is `private_gid`, the owner's user-private group
/// (sticky root-owned directories such as /tmp excepted). Directories
/// still to be created by `creator` must be created by a trusted user.
pub fn check_chain(
    chain: &[Owned],
    creator: Option<u32>,
    owner: u32,
    private_gid: Option<u32>,
) -> std::result::Result<(), String> {
    chain.iter().try_for_each(|dir| check_dir(dir, owner, private_gid))?;
    match creator {
        Some(uid) if uid != 0 && uid != owner => Err(format!(
            "cache directories would be created by uid {uid}, not by uid {owner} or root"
        )),
        _ => Ok(()),
    }
}

/// One path of the chain: trusted owner, no write access for others or for
/// a group other than the owner's user-private group.
fn check_dir(dir: &Owned, owner: u32, private_gid: Option<u32>) -> std::result::Result<(), String> {
    let path = dir.path.display();
    if dir.uid != 0 && dir.uid != owner {
        return Err(format!(
            "{path} is owned by uid {}, not by root or uid {owner}",
            dir.uid
        ));
    }
    if dir.mode & 0o1000 != 0 && dir.uid == 0 {
        return Ok(()); // sticky and root-owned, such as /tmp
    }
    if dir.mode & 0o002 != 0 {
        return Err(format!("{path} is writable by others"));
    }
    if dir.mode & 0o020 == 0 {
        return Ok(());
    }
    group_write(dir, owner, private_gid)
}

/// The group bits of `dir` allow write: trusted only for the owner's
/// private group, and never with an ACL, whose mask these bits are.
fn group_write(dir: &Owned, owner: u32, private_gid: Option<u32>) -> std::result::Result<(), String> {
    let path = dir.path.display();
    if dir.acl {
        return Err(format!(
            "{path} has an ACL that can give write access to other users; run `setfacl -b {path}`"
        ));
    }
    if Some(dir.gid) != private_gid {
        return Err(format!(
            "{path} is writable by group {}, which is not the private group of uid {owner}; run `chmod g-w {path}`",
            dir.gid
        ));
    }
    Ok(())
}

/// The owner's user-private group: the owner's primary group, when it has
/// the owner's name and lists no other member. Only that group may have
/// write access on a link target's chain.
/// `members` holds the accounts whose primary group it is, too
/// (`primary_members`).
pub fn user_private_gid(owner: &str, primary_gid: u32, group: &str, members: &[String]) -> Option<u32> {
    (group == owner && members.iter().all(|m| m == owner)).then_some(primary_gid)
}

/// `user_private_gid` of a uid from the account database; `None` when the
/// account or its group is unknown. Each uid is looked up once per run:
/// planning asks for every link, and a lookup can take a millisecond.
fn private_gid_of(owner: u32) -> Option<u32> {
    use nix::unistd::{Group, Uid, User};
    use std::collections::HashMap;
    thread_local!(static KNOWN: std::cell::RefCell<HashMap<u32, Option<u32>>> = Default::default());
    let lookup = || {
        let user = User::from_uid(Uid::from_raw(owner)).ok().flatten()?;
        let group = Group::from_gid(user.gid).ok().flatten()?;
        // shortcut: only /etc/passwd (where the musl build looks users up),
        // not NSS sources such as LDAP. Unreadable: no private group.
        let passwd = read_limited(Path::new("/etc/passwd"), OFlag::empty(), crate::config::MAX_FILE_SIZE).ok()?;
        let mut members = group.mem;
        members.extend(primary_members(&String::from_utf8_lossy(&passwd), user.gid.as_raw()));
        user_private_gid(&user.name, user.gid.as_raw(), &group.name, &members)
    };
    KNOWN.with_borrow_mut(|known| *known.entry(owner).or_insert_with(lookup))
}

/// True when `path` has a POSIX access ACL. Linux keeps its xattr only for
/// an ACL that says more than the mode bits.
/// shortcut: macOS ACLs are not read (they are no xattr); read them with
/// `acl_get_fd` if the trust rule must see them there.
pub fn has_acl(path: impl rustix::path::Arg) -> bool {
    rustix::fs::getxattr_query(path, "system.posix_acl_access").is_ok()
}

/// The accounts in `passwd` (the text of `/etc/passwd`) whose primary
/// group is `gid`. A line that does not parse is no account.
fn primary_members(passwd: &str, gid: u32) -> Vec<String> {
    passwd
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            (fields.nth(2)?.parse::<u32>().ok()? == gid).then(|| name.to_owned())
        })
        .collect()
}

/// Applies `check_chain` to the real path of a cache entry, which may not
/// exist yet (then the current user would create the missing part).
pub fn trust_cache(entry: &Path, owner: u32) -> std::result::Result<(), String> {
    let mut existing = entry;
    while fs::symlink_metadata(existing).is_err() {
        existing = existing.parent().ok_or("cache path has no existing ancestor")?;
    }
    let creator = (existing != entry).then(|| geteuid().as_raw());
    let real = real_path(existing).map_err(|e| format!("{}: {e}", existing.display()))?;
    trusted_chain(&real, owner)?;
    // The paths passed: only the creator is left to check.
    check_chain(&[], creator, owner, private_gid_of(owner))
}

/// `check_chain` of the real path `real` and of each directory above it,
/// the nearest first. The verdict of each path is kept for the run: the
/// sources of one tree share their directories, and a deep tree would
/// cost the square of its depth for each source.
fn trusted_chain(real: &Path, owner: u32) -> std::result::Result<(), String> {
    use std::collections::HashMap;
    type Verdicts = HashMap<(u32, PathBuf), std::result::Result<(), String>>;
    thread_local!(static KNOWN: std::cell::RefCell<Verdicts> = Default::default());
    let key = (owner, real.to_path_buf());
    if let Some(known) = KNOWN.with_borrow(|k| k.get(&key).cloned()) {
        return known;
    }
    let verdict = owned(real)
        .and_then(|path| check_chain(&[path], None, owner, private_gid_of(owner)))
        .and_then(|()| real.parent().map_or(Ok(()), |up| trusted_chain(up, owner)));
    KNOWN.with_borrow_mut(|k| k.insert(key, verdict.clone()));
    verdict
}

/// Owner, group, mode and ACL of `path`, following a symlink.
fn owned(path: &Path) -> std::result::Result<Owned, String> {
    let m = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Owned {
        path: path.to_path_buf(),
        uid: m.uid(),
        gid: m.gid(),
        mode: m.mode() & 0o7777,
        acl: has_acl(path),
    })
}

/// `fs::canonicalize` of `path`, with each directory resolved once for the
/// run: below a real directory, a name that is not a symlink is real too.
fn real_path(path: &Path) -> io::Result<PathBuf> {
    use std::collections::HashMap;
    thread_local!(static REAL: std::cell::RefCell<HashMap<PathBuf, PathBuf>> = Default::default());
    if let Some(real) = REAL.with_borrow(|r| r.get(path).cloned()) {
        return Ok(real);
    }
    let real = match (path.parent(), path.file_name()) {
        (Some(up), Some(name)) if up.is_absolute() && !fs::symlink_metadata(path)?.is_symlink() => {
            real_path(up)?.join(name)
        }
        _ => fs::canonicalize(path)?,
    };
    REAL.with_borrow_mut(|r| r.insert(path.to_path_buf(), real.clone()));
    Ok(real)
}

/// Owner of the deepest existing directory at or above `dir`.
pub fn owner_of(dir: &Path) -> Result<u32> {
    fs::metadata(dir)
        .map(|m| m.uid())
        .ctx(|| format!("inspect {}", dir.display()))
}

/// The program to start for `name`: a name with a `/` as given, else the
/// first executable file called `name` in `PATH`. Callers start this path,
/// so `Command` never searches `PATH` itself.
pub fn find_program(name: impl AsRef<OsStr>) -> Option<PathBuf> {
    let name = name.as_ref();
    if name.as_bytes().contains(&b'/') {
        return Some(PathBuf::from(name));
    }
    find_in(&std::env::var_os("PATH")?, name)
}

/// First executable file called `name` in a `PATH`-style list. An empty or
/// relative entry is skipped: it names the current directory (often the
/// repository), where anyone who can write a file could plant a program.
fn find_in(path_list: &OsStr, name: impl AsRef<Path>) -> Option<PathBuf> {
    std::env::split_paths(path_list)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(&name))
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
    fn remove_empty_tree_keeps_what_holds_anything_and_follows_no_symlink() {
        let (_t, d) = tmp();
        for dir in ["run/home/a/b", "run/cache/c", "outside/empty"] {
            fs::create_dir_all(d.join(dir)).unwrap();
        }
        fs::write(d.join("run/cache/c/saved"), "x").unwrap();
        std::os::unix::fs::symlink(d.join("outside"), d.join("run/home/link")).unwrap();
        remove_empty_tree(&d.join("run"));
        assert!(!d.join("run/home/a").exists(), "empty: removed");
        assert!(d.join("run/cache/c/saved").exists());
        assert!(d.join("outside/empty").is_dir(), "the symlink is not followed");
        assert!(d.join("run/home/link").is_symlink());
        remove_empty_tree(&d.join("outside"));
        assert!(!d.join("outside").exists(), "the whole tree, when all of it is empty");
    }

    /// Under `O_NOFOLLOW` a symlink is "not a regular file"; without it,
    /// the file it points at is read. Another failed open, such as a
    /// missing file, keeps its own error.
    #[test]
    fn read_limited_refuses_a_symlink_only_when_it_does_not_follow_it() {
        let (_t, d) = tmp();
        fs::write(d.join("target"), "x").unwrap();
        symlink(d.join("target"), d.join("link")).unwrap();
        assert_eq!(read_limited(&d.join("link"), OFlag::empty(), 8).unwrap(), b"x");
        let refused = read_limited(&d.join("link"), OFlag::O_NOFOLLOW, 8).unwrap_err();
        assert!(matches!(refused, Error::Config(_)), "{refused}");
        let missing = read_limited(&d.join("missing"), OFlag::O_NOFOLLOW, 8).unwrap_err();
        assert!(matches!(missing, Error::Io { .. }), "{missing}");
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
    fn write_new_never_replaces_anything() {
        let (_t, d) = tmp();
        write_new(&d.join("f"), b"one", 0o640).unwrap();
        assert_eq!(mode_of(&d.join("f")), 0o640);
        assert!(write_new(&d.join("f"), b"two", 0o644).is_err());
        assert_eq!(fs::read(d.join("f")).unwrap(), b"one");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no temporary file left");
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

    /// `d`, walked for the current user.
    fn open(d: &Path) -> Dir {
        open_ready(d, geteuid().as_raw()).unwrap()
    }

    fn name(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    /// A temporary name for the tests; no journal is set.
    const T: &str = ".rustow-tmp-test";

    /// `Dir::write` of `bytes` with `mode`.
    fn put(dir: &Dir, name: &str, bytes: &[u8], mode: u32, replace: bool) -> Result<()> {
        let meta = FileMeta {
            mode: Some(mode),
            ..FileMeta::default()
        };
        let want = (&Hash::of(bytes), bytes.len() as u64);
        dir.write(OsStr::new(name), T, &mut &bytes[..], want, meta, replace)
    }

    #[test]
    fn new_file_and_link_never_replace_anything() {
        let (_t, d) = tmp();
        let dir = open(&d);
        put(&dir, "f", b"one", 0o640, false).unwrap();
        assert_eq!(fs::read(d.join("f")).unwrap(), b"one");
        assert_eq!(mode_of(&d.join("f")), 0o640);
        let e = put(&dir, "f", b"two", 0o644, false).unwrap_err();
        assert!(
            e.to_string().starts_with(&format!("replace {}", d.join("f").display())),
            "{e}"
        );
        assert_eq!(fs::read(d.join("f")).unwrap(), b"one");
        dir.symlink(name("l"), T, Path::new("t"), false).unwrap();
        assert!(dir.symlink(name("l"), T, Path::new("u"), false).is_err());
        assert!(dir.symlink(name("f"), T, Path::new("u"), false).is_err());
        assert_eq!(fs::read_link(d.join("l")).unwrap(), PathBuf::from("t"));
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2, "no temporary file left");
    }

    #[test]
    fn write_atomic_refuses_directory() {
        let (_t, d) = tmp();
        fs::create_dir(d.join("dir")).unwrap();
        assert!(write_atomic(&d.join("dir"), b"x", 0o644, None, None).is_err());
        assert!(d.join("dir").is_dir());
    }

    #[test]
    fn symlink_replace_replaces_symlink_but_not_directory() {
        let (_t, d) = tmp();
        let dir = open(&d);
        dir.symlink(name("l"), T, Path::new("a"), true).unwrap();
        dir.symlink(name("l"), T, Path::new("b"), true).unwrap();
        assert_eq!(fs::read_link(d.join("l")).unwrap(), PathBuf::from("b"));
        fs::create_dir(d.join("dir")).unwrap();
        assert!(dir.symlink(name("dir"), T, Path::new("b"), true).is_err());
        assert!(d.join("dir").is_dir());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2);
    }

    #[test]
    fn set_meta_changes_mode_and_refuses_symlink() {
        let (_t, d) = tmp();
        let dir = open(&d);
        let mode = |m| FileMeta {
            mode: Some(m),
            ..Default::default()
        };
        fs::write(d.join("f"), b"x").unwrap();
        dir.set_meta(name("f"), mode(0o600), |_| Ok(())).unwrap();
        assert_eq!(mode_of(&d.join("f")), 0o600);
        symlink(d.join("f"), d.join("l")).unwrap();
        assert!(dir.set_meta(name("l"), mode(0o644), |_| Ok(())).is_err());
        assert_eq!(mode_of(&d.join("f")), 0o600);
        let refused = dir.set_meta(name("f"), mode(0o640), |_| Err(Error::Failed("no".into())));
        assert!(refused.is_err());
        assert_eq!(mode_of(&d.join("f")), 0o600, "a refused check changes nothing");
    }

    /// The check sees the open file, and the change reaches that file, also
    /// when another file takes its name after the check (a hardlink swap).
    #[test]
    fn set_meta_changes_the_file_that_its_check_saw() {
        let (_t, d) = tmp();
        let dir = open(&d);
        fs::write(d.join("f"), b"mine").unwrap();
        fs::hard_link(d.join("f"), d.join("kept")).unwrap();
        fs::write(d.join("other"), b"theirs").unwrap();
        fs::set_permissions(d.join("other"), fs::Permissions::from_mode(0o644)).unwrap();
        let meta = FileMeta {
            mode: Some(0o600),
            ..Default::default()
        };
        let mut seen = None;
        dir.set_meta(name("f"), meta, |now| {
            seen = Some(now);
            fs::rename(d.join("other"), d.join("f")).map_err(|e| Error::Failed(e.to_string()))
        })
        .unwrap();
        assert!(matches!(seen, Some(Observed::File { hash, .. }) if hash == Hash::of(b"mine")));
        assert_eq!(mode_of(&d.join("f")), 0o644, "the swapped-in file is not changed");
        assert_eq!(mode_of(&d.join("kept")), 0o600, "the checked file is");
    }

    #[test]
    fn backup_moves_into_an_existing_dir_only() {
        let (_t, d) = tmp();
        let (dir, me) = (open(&d), geteuid().as_raw());
        fs::write(d.join("f"), b"data").unwrap();
        symlink("t", d.join("l")).unwrap();
        assert!(backup(&dir, name("f"), &d.join("bk/a/f"), me, T, false).is_err());
        assert!(!d.join("bk").exists() && d.join("f").exists(), "it makes no directory");
        ensure_private_dir(&d.join("bk/a")).unwrap();
        backup(&dir, name("f"), &d.join("bk/a/f"), me, T, false).unwrap();
        backup(&dir, name("l"), &d.join("bk/a/l"), me, T, false).unwrap();
        assert!(!d.join("f").exists());
        assert_eq!(fs::read(d.join("bk/a/f")).unwrap(), b"data");
        assert_eq!(fs::read_link(d.join("bk/a/l")).unwrap(), PathBuf::from("t"));
        // Never overwrite an existing backup.
        fs::write(d.join("f"), b"again").unwrap();
        let e = backup(&dir, name("f"), &d.join("bk/a/f"), me, T, false).unwrap_err();
        assert!(e.to_string().ends_with("bk/a/f already exists"), "{e}");
        assert_eq!(fs::read(d.join("bk/a/f")).unwrap(), b"data");
    }

    /// A private backup (of a file that `me` could not see) keeps only the
    /// owner bits of the file that it moves. When the move fails, the file
    /// gets its mode back. A symlink moves as it is.
    #[test]
    fn a_private_backup_keeps_only_the_owner_bits() {
        let (_t, d) = tmp();
        let (dir, me) = (open(&d), geteuid().as_raw());
        fs::write(d.join("f"), b"data").unwrap();
        fs::set_permissions(d.join("f"), fs::Permissions::from_mode(0o754)).unwrap();
        symlink("t", d.join("l")).unwrap();
        ensure_private_dir(&d.join("bk")).unwrap();
        fs::write(d.join("bk/taken"), b"older").unwrap();
        assert!(backup(&dir, name("f"), &d.join("bk/taken"), me, T, true).is_err());
        assert_eq!(mode_of(&d.join("f")), 0o754, "the mode comes back");
        backup(&dir, name("f"), &d.join("bk/f"), me, T, true).unwrap();
        assert_eq!(mode_of(&d.join("bk/f")), 0o700);
        backup(&dir, name("l"), &d.join("bk/l"), me, T, true).unwrap();
        assert_eq!(fs::read_link(d.join("bk/l")).unwrap(), PathBuf::from("t"));
    }

    #[test]
    fn mkdir_creates_or_accepts_a_trusted_directory_only() {
        let (_t, d) = tmp();
        let (dir, me) = (open(&d), geteuid().as_raw());
        let (made, created) = dir.mkdir(name("a"), 0o755, me).unwrap();
        assert!(created && made.path == d.join("a") && d.join("a").is_dir());
        let (again, created) = dir.mkdir(name("a"), 0o755, me).unwrap();
        assert!(!created && again.id().unwrap() == made.id().unwrap());
        fs::set_permissions(d.join("a"), fs::Permissions::from_mode(0o777)).unwrap();
        let open_dir = dir.mkdir(name("a"), 0o755, me).unwrap_err().to_string();
        assert!(open_dir.contains("writable by others"), "{open_dir}");
        symlink(d.join("a"), d.join("l")).unwrap();
        assert!(dir.mkdir(name("l"), 0o755, me).is_err(), "a symlink is no directory");
        for bad in ["x/y", "..", "."] {
            let e = dir.mkdir(name(bad), 0o755, me).unwrap_err().to_string();
            assert!(e.ends_with("not a directory name"), "{bad}: {e}");
        }
        assert!(!d.join("x").exists());
    }

    #[test]
    fn remove_empty_stops_at_the_first_directory_that_stays() {
        let (_t, d) = tmp();
        fs::create_dir_all(d.join("a/b/c")).unwrap();
        fs::write(d.join("a/keep"), b"").unwrap();
        let dirs = [d.join("a/b/c"), d.join("a/b"), d.join("a")];
        let removed = remove_empty(dirs.iter().map(PathBuf::as_path), geteuid().as_raw());
        assert_eq!(removed, dirs[..2]);
        assert!(d.join("a/keep").exists() && !d.join("a/b").exists());
    }

    #[test]
    fn private_tree_refuses_a_symlink_at_any_level() {
        let (_t, d) = tmp();
        fs::create_dir(d.join("elsewhere")).unwrap();
        let dir = private_tree(&d.join("c"), &d.join("c/x/y")).unwrap();
        assert_eq!(dir.path, d.join("c/x/y"));
        assert_eq!(mode_of(&d.join("c/x/y")), 0o700);
        put(&dir, "f", b"in", 0o600, true).unwrap();
        assert_eq!(fs::read(d.join("c/x/y/f")).unwrap(), b"in");
        std::os::unix::fs::symlink(d.join("elsewhere"), d.join("c/l")).unwrap();
        assert!(private_tree(&d.join("c"), &d.join("c/l/y")).is_err());
        assert_eq!(fs::read_dir(d.join("elsewhere")).unwrap().count(), 0);
        assert!(private_tree(&d.join("c"), &d.join("other")).is_err());
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
        if !geteuid().is_root() {
            let e = ensure_private_dir(Path::new("/usr")).unwrap_err().to_string();
            assert!(e.contains("/usr is owned by uid 0, not by the current user"), "{e}");
        }
    }

    fn dir_id(p: &Path) -> DirId {
        let st = nix::sys::stat::stat(p).unwrap();
        DirId {
            dev: st.st_dev,
            ino: st.st_ino,
        }
    }

    #[test]
    fn parent_state_cases() {
        let (_t, d) = tmp();
        let state = |dest: &str| parent_state(&d.join("h"), &rel(dest), geteuid().as_raw()).unwrap();
        let reason = |dest: &str| match state(dest) {
            ParentState::Blocked { reason, .. } => reason,
            other => panic!("{dest}: {other:?}"),
        };
        let ready = |dir: PathBuf| ParentState::Ready { id: dir_id(&dir), dir };
        fs::create_dir_all(d.join("h/.config")).unwrap();
        assert_eq!(state(".bashrc"), ready(d.join("h")));
        assert_eq!(
            state(".config/nvim/lua/init.lua"),
            ParentState::Missing {
                existing: d.join("h/.config"),
                id: dir_id(&d.join("h/.config")),
                missing: vec!["nvim".into(), "lua".into()]
            }
        );
        // Safe symlinked ancestors are followed, also relative ones with `..`.
        fs::create_dir_all(d.join("data/cfg")).unwrap();
        symlink(d.join("data"), d.join("h/.local")).unwrap();
        assert_eq!(state(".local/cfg/x"), ready(d.join("data/cfg")));
        symlink("../data/./cfg", d.join("h/rel")).unwrap();
        assert_eq!(state("rel/x"), ready(d.join("data/cfg")));
        // A file, a dangling link, a loop and a world-writable target block.
        fs::write(d.join("h/file"), b"").unwrap();
        assert_eq!(reason("file/x"), "not a directory");
        symlink(d.join("gone"), d.join("h/dangling")).unwrap();
        assert_eq!(reason("dangling/x"), "symlink does not resolve");
        symlink("loop", d.join("h/loop")).unwrap();
        assert!(reason("loop/x").contains("symbolic links"), "{}", reason("loop/x"));
        fs::create_dir(d.join("open")).unwrap();
        fs::set_permissions(d.join("open"), fs::Permissions::from_mode(0o777)).unwrap();
        symlink(d.join("open"), d.join("h/open")).unwrap();
        let open = reason("open/x");
        assert!(
            open.starts_with("symlink target is not safe") && open.contains("writable by others"),
            "{open}"
        );
    }

    /// A parent that the user cannot search is still a known directory: its
    /// entries are `Hidden`, for the sudo helper to look at. macOS has no
    /// `O_PATH`, so the walk cannot open it there; it must not block.
    #[test]
    fn a_parent_that_you_cannot_search_is_ready_and_its_entries_are_hidden() {
        if geteuid().is_root() {
            eprintln!("SKIP: root searches every directory");
            return;
        }
        let (_t, d) = tmp();
        fs::create_dir_all(d.join("h/sealed/sub")).unwrap();
        let id = dir_id(&d.join("h/sealed"));
        fs::set_permissions(d.join("h/sealed"), fs::Permissions::from_mode(0o000)).unwrap();
        let me = geteuid().as_raw();
        let ready = parent_state(&d.join("h"), &rel("sealed/x"), me);
        let deeper = parent_state(&d.join("h"), &rel("sealed/sub/x"), me);
        let seen = inspect(&d.join("h/sealed/x"));
        let below = inspect(&d.join("h/sealed/sub/x"));
        fs::set_permissions(d.join("h/sealed"), fs::Permissions::from_mode(0o755)).unwrap();
        let dir = d.join("h/sealed");
        assert_eq!(ready.unwrap(), ParentState::Ready { dir: dir.clone(), id });
        assert_eq!(seen.unwrap(), Observed::Hidden);
        assert!(below.is_err(), "{below:?}");
        match deeper.unwrap() {
            ParentState::Blocked { path, reason } => {
                assert!(path.starts_with(&dir), "{path:?}");
                assert_eq!(reason, nix::errno::Errno::EACCES.desc());
            }
            other => panic!("{other:?}"),
        }
    }

    /// The planned directory must still have the planned identity, open or
    /// sealed; a sealed one that is still planned is denied, not "changed".
    #[test]
    fn a_planned_directory_is_checked_by_its_identity() {
        if geteuid().is_root() {
            eprintln!("SKIP: root searches every directory");
            return;
        }
        let (_t, d) = tmp();
        fs::create_dir_all(d.join("open")).unwrap();
        fs::create_dir_all(d.join("sealed")).unwrap();
        let (open, sealed) = (d.join("open"), d.join("sealed"));
        let (open_id, sealed_id) = (dir_id(&open), dir_id(&sealed));
        let me = geteuid().as_raw();
        let changed = |r: Result<()>| r.unwrap_err().to_string().contains("changed while rustow was running");
        assert!(check_planned(&open, open_id, me).is_ok());
        assert!(changed(check_planned(&open, sealed_id, me)));
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
        let checked = (
            check_planned(&sealed, sealed_id, me),
            check_planned(&sealed, open_id, me),
        );
        let opened = (
            open_planned(&sealed, sealed_id, me).map(drop),
            open_planned(&sealed, open_id, me).map(drop),
        );
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o755)).unwrap();
        // Linux opens it (`O_PATH`); macOS gets its identity without opening it.
        assert!(checked.0.is_ok(), "{:?}", checked.0);
        assert!(changed(checked.1));
        assert!(changed(opened.1));
        if let Err(e) = opened.0 {
            assert!(e.to_string().ends_with(nix::errno::Errno::EACCES.desc()), "{e}");
        }
    }

    /// A symlinked ancestor must be owned by root or by the user, as each
    /// path on a link target's chain: another user could point it elsewhere.
    #[test]
    fn a_symlinked_ancestor_is_followed_only_when_its_owner_is_trusted() {
        let me = geteuid().as_raw();
        if me == 0 {
            eprintln!("SKIP: root owns every symlink it creates");
            return;
        }
        let (_t, d) = tmp();
        fs::create_dir(d.join("h")).unwrap();
        // /usr is root-owned and not writable by others on Linux and macOS.
        symlink("/usr", d.join("h/sys")).unwrap();
        let ready = parent_state(&d.join("h"), &rel("sys/x"), me).unwrap();
        assert!(matches!(ready, ParentState::Ready { .. }), "{ready:?}");
        match parent_state(&d.join("h"), &rel("sys/x"), me + 1).unwrap() {
            ParentState::Blocked { path, reason } => {
                assert_eq!(path, d.join("h/sys"));
                assert!(reason.contains(&format!("owned by uid {me}")), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mkdir_refuses_a_file_in_its_place() {
        let (_t, d) = tmp();
        fs::write(d.join("f"), "x").unwrap();
        let e = open(&d)
            .mkdir(name("f"), 0o700, geteuid().as_raw())
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with(&format!("create directory {}", d.join("f").display())),
            "{e}"
        );
    }

    /// A path whose group has the same id as its owner (a user-private group).
    fn owned(path: &str, uid: u32, mode: u32) -> Owned {
        Owned {
            path: path.into(),
            uid,
            gid: uid,
            mode,
            acl: false,
        }
    }

    #[test]
    fn chain_trusted_when_owned_by_root_or_owner() {
        let chain = [
            owned("/", 0, 0o755),
            owned("/var", 0, 0o755),
            owned("/var/cache/rustow", 0, 0o700),
        ];
        assert!(check_chain(&chain, None, 0, None).is_ok());
        let user = [
            owned("/", 0, 0o755),
            owned("/home/u", 1000, 0o700),
            owned("/home/u/dot", 1000, 0o755),
        ];
        assert!(check_chain(&user, Some(1000), 1000, None).is_ok());
    }

    #[test]
    fn chain_rejects_foreign_owner() {
        let chain = [owned("/", 0, 0o755), owned("/home/u", 1000, 0o700)];
        let e = check_chain(&chain, None, 0, None).unwrap_err();
        assert!(e.contains("/home/u") && e.contains("1000"), "{e}");
    }

    #[test]
    fn chain_rejects_writable_by_others_but_allows_sticky_root() {
        let open = [owned("/", 0, 0o755), owned("/shared", 0, 0o775)];
        assert!(check_chain(&open, None, 0, None).unwrap_err().contains("writable"));
        let tmp = [
            owned("/", 0, 0o755),
            owned("/tmp", 0, 0o1777),
            owned("/tmp/x", 1000, 0o700),
        ];
        assert!(check_chain(&tmp, None, 1000, None).is_ok());
        let sticky_user = [owned("/", 0, 0o755), owned("/tmp", 1000, 0o1777)];
        assert!(check_chain(&sticky_user, None, 1000, None).is_err());
    }

    #[test]
    fn chain_rejects_untrusted_creator() {
        let chain = [owned("/", 0, 0o755)];
        assert!(check_chain(&chain, Some(1000), 0, None).unwrap_err().contains("1000"));
        assert!(check_chain(&chain, Some(0), 0, None).is_ok());
        assert!(
            check_chain(&chain, Some(0), 1000, None).is_ok(),
            "root may create cache dirs for anyone"
        );
    }

    #[test]
    fn user_private_group_rule() {
        let names = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            user_private_gid("alice", 1000, "alice", &[]),
            Some(1000),
            "user-private group"
        );
        assert_eq!(
            user_private_gid("alice", 1000, "alice", &names(&["alice"])),
            Some(1000),
            "the owner may be listed"
        );
        assert_eq!(
            user_private_gid("alice", 100, "users", &names(&["alice", "bob"])),
            None,
            "shared group"
        );
        assert_eq!(
            user_private_gid("alice", 1000, "alice", &names(&["bob"])),
            None,
            "extra member"
        );
        assert_eq!(user_private_gid("alice", 1000, "staff", &[]), None, "name mismatch");
    }

    /// An account whose primary group is the owner's group is a member
    /// too, also when the group does not list it.
    #[test]
    fn primary_members_are_the_accounts_with_that_primary_gid() {
        let passwd = "root:x:0:0::/root:/bin/sh\nalice:x:1000:1000::/home/alice:/bin/sh\n\
                      mallory:x:1001:1000::/home/mallory:/bin/sh\nbad line\nodd:x:1002:x::/:/bin/sh\n";
        assert_eq!(primary_members(passwd, 1000), ["alice", "mallory"]);
        assert_eq!(primary_members(passwd, 0), ["root"]);
        let members = primary_members(passwd, 1000);
        assert_eq!(user_private_gid("alice", 1000, "alice", &members), None);
    }

    #[test]
    fn chain_trusts_group_write_only_in_the_owners_private_group() {
        let dot = owned("/home/u/dot", 1000, 0o775);
        let chain = [owned("/", 0, 0o755), owned("/home/u", 1000, 0o700), dot.clone()];
        assert!(
            check_chain(&chain, None, 1000, Some(1000)).is_ok(),
            "umask 002 repository"
        );
        let e = check_chain(&chain, None, 1000, None).unwrap_err();
        assert!(e.contains("chmod g-w /home/u/dot"), "no private group: {e}");
        let shared = [Owned { gid: 100, ..dot }];
        let e = check_chain(&shared, None, 1000, Some(1000)).unwrap_err();
        assert!(e.contains("/home/u/dot") && e.contains("chmod g-w /home/u/dot"), "{e}");
        let world = [owned("/home/u/dot", 1000, 0o777)];
        let e = check_chain(&world, None, 1000, Some(1000)).unwrap_err();
        assert!(e.contains("writable by others") && !e.contains("chmod g-w"), "{e}");
    }

    /// With an ACL, the group bits are its mask: the most that named users
    /// and groups may do. A private owning group does not make it safe.
    #[test]
    fn chain_refuses_an_acl_whose_mask_allows_write() {
        let acl = |mode| {
            [Owned {
                acl: true,
                ..owned("/home/u/dot/f", 1000, mode)
            }]
        };
        let e = check_chain(&acl(0o664), None, 1000, Some(1000)).unwrap_err();
        assert_eq!(
            e,
            "/home/u/dot/f has an ACL that can give write access to other users; run `setfacl -b /home/u/dot/f`"
        );
        assert!(
            check_chain(&acl(0o644), None, 1000, Some(1000)).is_ok(),
            "mask without write"
        );
    }

    #[test]
    fn trust_cache_counts_the_creator_only_for_missing_parts() {
        if geteuid().is_root() {
            return;
        }
        // /usr is root-owned and not writable by others on Linux and macOS.
        assert!(trust_cache(Path::new("/usr"), 0).is_ok());
        let e = trust_cache(Path::new("/usr/rustow-no-such-dir/entry"), 0).unwrap_err();
        assert!(e.contains("would be created by uid"), "{e}");
    }

    /// The chain is the one of the real path, also behind a symlink, and
    /// the nearest path that the rule refuses gives the reason.
    #[test]
    fn trust_cache_checks_the_real_path_nearest_first() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        fs::create_dir_all(base.join("real/sub")).unwrap();
        fs::write(base.join("real/sub/f"), "x").unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        for dir in ["real", "real/sub"] {
            fs::set_permissions(base.join(dir), fs::Permissions::from_mode(0o777)).unwrap();
        }
        let me = geteuid().as_raw();
        let open = |dir: &str| format!("{} is writable by others", base.join(dir).display());
        assert_eq!(trust_cache(&base.join("link/sub/f"), me), Err(open("real/sub")));
        assert_eq!(trust_cache(&base.join("link/sub/new"), me), Err(open("real/sub")));
        assert_eq!(trust_cache(&base.join("link/new"), me), Err(open("real")));
    }

    #[test]
    fn backup_moves_across_filesystems() {
        let shm = Path::new("/dev/shm");
        // Not the build directory: it may be elsewhere (CARGO_TARGET_DIR) or missing.
        let local = std::env::temp_dir();
        let device = |p: &Path| fs::metadata(p).map(|m| m.dev()).ok();
        if !shm.is_dir() || device(shm) == device(&local) {
            return; // needs two filesystems (Linux /dev/shm and the temporary directory)
        }
        let src = tempfile::tempdir_in(shm).unwrap();
        let dst = tempfile::tempdir_in(&local).unwrap();
        fs::write(src.path().join("f"), b"data").unwrap();
        fs::set_permissions(src.path().join("f"), fs::Permissions::from_mode(0o640)).unwrap();
        symlink("t", src.path().join("l")).unwrap();
        let (from, me) = (open(src.path()), geteuid().as_raw());
        ensure_private_dir(&dst.path().join("bk")).unwrap();
        backup(&from, name("f"), &dst.path().join("bk/f"), me, T, false).unwrap();
        backup(&from, name("l"), &dst.path().join("bk/l"), me, T, false).unwrap();
        assert!(!src.path().join("f").exists() && fs::symlink_metadata(src.path().join("l")).is_err());
        assert_eq!(fs::read(dst.path().join("bk/f")).unwrap(), b"data");
        assert_eq!(mode_of(&dst.path().join("bk/f")), 0o640);
        assert_eq!(fs::read_link(dst.path().join("bk/l")).unwrap(), PathBuf::from("t"));
        // A private copy has only the owner bits.
        fs::write(src.path().join("p"), b"data").unwrap();
        fs::set_permissions(src.path().join("p"), fs::Permissions::from_mode(0o640)).unwrap();
        backup(&from, name("p"), &dst.path().join("bk/p"), me, T, true).unwrap();
        assert_eq!(mode_of(&dst.path().join("bk/p")), 0o600);
    }

    #[test]
    fn find_in_requires_an_executable_file() {
        let (_t, d) = tmp();
        fs::create_dir(d.join("bin")).unwrap();
        fs::write(d.join("bin/tool"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("bin/tool"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::create_dir(d.join("bin/dir")).unwrap();
        fs::set_permissions(d.join("bin/dir"), fs::Permissions::from_mode(0o755)).unwrap();
        let list = std::env::join_paths([d.join("missing"), d.join("bin")]).unwrap();
        assert_eq!(find_in(&list, "tool"), None, "not executable");
        assert_eq!(find_in(&list, "dir"), None, "a directory is not a program");
        fs::set_permissions(d.join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_in(&list, "tool"), Some(d.join("bin/tool")));
    }

    #[test]
    fn find_in_skips_entries_that_are_not_absolute() {
        let (_t, d) = tmp();
        fs::create_dir(d.join("bin")).unwrap();
        fs::write(d.join("bin/tool"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
        // The same directory, named relative to the current directory.
        let up: PathBuf = std::env::current_dir()
            .unwrap()
            .components()
            .skip(1)
            .map(|_| "..")
            .collect();
        let relative = up.join(d.join("bin").strip_prefix("/").unwrap());
        assert!(relative.join("tool").is_file(), "{}", relative.display());
        let list = std::env::join_paths([Path::new(""), Path::new("."), &relative]).unwrap();
        assert_eq!(find_in(&list, "tool"), None);
        let list = std::env::join_paths([Path::new(""), &relative, &d.join("bin")]).unwrap();
        assert_eq!(find_in(&list, "tool"), Some(d.join("bin/tool")));
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
        // The first day of a year in the previous 400-year era, after its first century.
        assert_eq!(format_utc(920_246_400), "19990301T000000Z");
    }
}
