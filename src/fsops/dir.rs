//! Directories held open. rustow resolves a destination's directory one
//! component at a time and keeps it open, so that a path changed after
//! the check cannot send a later change somewhere else. Every change is
//! made relative to an open directory.

use std::ffi::OsStr;
use std::fs::{self, File, Permissions};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nix::errno::Errno;
use nix::fcntl::{AT_FDCWD, AtFlags, OFlag, openat, readlinkat, renameat};
use nix::sys::stat::{FileStat, Mode, SFlag, fchmod, fstat, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, linkat, symlinkat, unlinkat};
use serde::{Deserialize, Serialize};

use super::{FileMeta, Observed, Owned, TEMP_PREFIX, check_dir, private_gid_of};
use crate::error::{Error, IoContext, Result};
use crate::hash::Hash;

/// Device and inode of a directory: the plan keeps it, and the executor
/// refuses a directory with another identity at the planned path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirId {
    pub dev: nix::libc::dev_t,
    pub ino: nix::libc::ino_t,
}

/// An open directory and its real path (for messages and the plan).
#[derive(Debug)]
pub struct Dir {
    fd: OwnedFd,
    pub path: PathBuf,
}

/// Where a walk ended.
#[derive(Debug)]
pub enum Walked {
    Ready(Dir),
    /// `dir` is the deepest existing directory; `missing` names the rest.
    Missing {
        dir: Dir,
        missing: Vec<String>,
    },
    /// A component is not a directory, cannot be opened, or is a symlink
    /// that the trust rule refuses.
    Blocked {
        path: PathBuf,
        reason: String,
    },
}

/// Most symlinks that one walk follows, as in the kernel.
const MAX_SYMLINKS: u32 = 40;

/// Opens a directory only to walk below it: no read permission needed.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SEARCH: OFlag = OFlag::O_PATH;
#[cfg(target_vendor = "apple")]
const SEARCH: OFlag = OFlag::O_SEARCH;
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
const SEARCH: OFlag = OFlag::O_RDONLY;

/// One step of a walk.
enum Next {
    Into(Dir),
    Missing,
    Blocked(PathBuf, String),
}

/// The planned directory at `path`, walked again for `me`; any other
/// directory there (another inode, a missing or blocked path) fails.
pub fn open_planned(path: &Path, id: DirId, me: u32) -> Result<Dir> {
    match Dir::open(path, me)? {
        Walked::Ready(dir) if dir.id()? == id => Ok(dir),
        _ => Err(Error::Failed(format!(
            "{} changed while rustow was running; left unchanged",
            path.display()
        ))),
    }
}

/// The existing directory at `path`, walked for `me`.
pub fn open_ready(path: &Path, me: u32) -> Result<Dir> {
    match Dir::open(path, me)? {
        Walked::Ready(dir) => Ok(dir),
        Walked::Missing { .. } => Err(Error::Failed(format!("{}: {}", path.display(), Errno::ENOENT.desc()))),
        Walked::Blocked { path, reason } => Err(Error::Failed(format!("{}: {reason}", path.display()))),
    }
}

impl Dir {
    /// Walks the absolute path `abs` from `/` (see `walk`).
    pub fn open(abs: &Path, me: u32) -> Result<Walked> {
        if !abs.is_absolute() {
            return Err(Error::Failed(format!("{} is not an absolute path", abs.display())));
        }
        Dir::root()?.walk(abs, me)
    }

    fn root() -> Result<Dir> {
        let fd = sys(openat(
            AT_FDCWD,
            "/",
            SEARCH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        ))
        .ctx(|| "open /".to_owned())?;
        Ok(Dir { fd, path: "/".into() })
    }

    /// The directory at `path`, opened as the kernel resolves it (symlinks
    /// followed): only to read an entry, never to change one.
    pub(super) fn at(path: &Path) -> io::Result<Dir> {
        let fd = sys(openat(
            AT_FDCWD,
            path,
            SEARCH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        ))?;
        Ok(Dir {
            fd,
            path: path.to_path_buf(),
        })
    }

    /// Walks `rel` below this directory. Each component opens without
    /// following a symlink. A symlink is followed only when `check_dir`
    /// accepts its owner (for `me`) and the directory that it reaches, and
    /// it is still the same symlink after that; at most 40 per walk.
    pub fn walk(self, rel: &Path, me: u32) -> Result<Walked> {
        let mut budget = MAX_SYMLINKS;
        walk_in(self, rel, me, &mut budget)
    }

    /// Device and inode of this directory.
    pub fn id(&self) -> Result<DirId> {
        let st = sys(fstat(&self.fd)).ctx(|| format!("inspect {}", self.path.display()))?;
        Ok(id_of(&st))
    }

    fn entry(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }

    /// The directory `name` (`..` too), opened with `flags`, never through a symlink.
    fn child(&self, name: &OsStr, flags: OFlag) -> nix::Result<Dir> {
        let flags = flags | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let fd = openat(&self.fd, name, flags, Mode::empty())?;
        let path = match name == ".." {
            true => self.path.parent().unwrap_or(&self.path).to_path_buf(),
            false => self.entry(name),
        };
        Ok(Dir { fd, path })
    }

    /// One component: `..`, a directory, a missing entry or a symlink.
    fn step(&self, name: &OsStr, me: u32, budget: &mut u32) -> Result<Next> {
        match self.child(name, SEARCH) {
            Ok(dir) => Ok(Next::Into(dir)),
            Err(Errno::ENOENT) => Ok(Next::Missing),
            Err(Errno::ELOOP | Errno::ENOTDIR) => self.follow(name, self.entry(name), me, budget),
            Err(e) => Ok(Next::Blocked(self.entry(name), e.desc().into())),
        }
    }

    /// Follows the symlink `name` (at `path`) if the trust rule allows it.
    fn follow(&self, name: &OsStr, path: PathBuf, me: u32, budget: &mut u32) -> Result<Next> {
        let link = match self.followable(name, &path, me, budget) {
            Ok(link) => link,
            Err(why) => return Ok(Next::Blocked(path, why)),
        };
        match self.reach(name, &path, me, budget)? {
            Ok(target) => self.settle(name, path, &link, target, me),
            Err(blocked) => Ok(blocked),
        }
    }

    /// `target`, the directory that the symlink `link` at `name` reaches,
    /// when `check_dir` accepts it and `name` is still that symlink.
    fn settle(&self, name: &OsStr, path: PathBuf, link: &FileStat, target: Dir, me: u32) -> Result<Next> {
        let st = sys(fstat(&target.fd)).ctx(|| format!("inspect {}", target.path.display()))?;
        if let Err(why) = trusted(&target.path, &st, st.st_mode as u32 & 0o7777, me) {
            return Ok(Next::Blocked(path, format!("symlink target is not safe: {why}")));
        }
        match fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(again) if id_of(&again) == id_of(link) => Ok(Next::Into(target)),
            _ => Ok(Next::Blocked(path, "changed while rustow was running".into())),
        }
    }

    /// The symlink `name` (at `path`), when a walk may follow it: `check_dir`
    /// accepts its owner, and the walk has not followed 40 symlinks yet.
    fn followable(
        &self,
        name: &OsStr,
        path: &Path,
        me: u32,
        budget: &mut u32,
    ) -> std::result::Result<FileStat, String> {
        let link = match fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(st) if kind(&st) == SFlag::S_IFLNK => st,
            Ok(_) => return Err("not a directory".into()),
            Err(e) => return Err(e.desc().into()),
        };
        // A symlink's own mode bits mean nothing: only its owner counts.
        trusted(path, &link, 0, me).map_err(|why| format!("symlink is not safe: {why}"))?;
        *budget = budget.checked_sub(1).ok_or(Errno::ELOOP.desc())?;
        Ok(link)
    }

    /// The directory that the text of the symlink `name` (at `path`) names,
    /// walked with the same rule; else the step that blocks.
    fn reach(&self, name: &OsStr, path: &Path, me: u32, budget: &mut u32) -> Result<std::result::Result<Dir, Next>> {
        let blocked = |reason: &str| Ok(Err(Next::Blocked(path.to_path_buf(), reason.into())));
        let text = match readlinkat(&self.fd, name) {
            Ok(t) => t,
            Err(e) => return blocked(e.desc()),
        };
        let here = self.fd.try_clone().ctx(|| format!("open {}", self.path.display()))?;
        let start = Dir {
            fd: here,
            path: self.path.clone(),
        };
        match walk_in(start, Path::new(&text), me, budget)? {
            Walked::Ready(dir) => Ok(Ok(dir)),
            Walked::Missing { .. } => blocked("symlink does not resolve"),
            Walked::Blocked { path, reason } => Ok(Err(Next::Blocked(path, reason))),
        }
    }

    /// What is at `name` now, without following a symlink there.
    pub fn inspect(&self, name: &OsStr) -> Result<Observed> {
        let Some(st) = self.stat(name)? else {
            return Ok(Observed::Missing);
        };
        Ok(match kind(&st) {
            SFlag::S_IFLNK => Observed::Symlink(self.link_text(name)?.into()),
            SFlag::S_IFDIR => Observed::Dir,
            SFlag::S_IFREG => self.observe_file(name)?,
            _ => Observed::Other,
        })
    }

    /// `fstatat` of `name` without following a symlink; `None` when it is missing.
    fn stat(&self, name: &OsStr) -> Result<Option<FileStat>> {
        match fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Err(Errno::ENOENT) => Ok(None),
            other => sys(other)
                .map(Some)
                .ctx(|| format!("inspect {}", self.entry(name).display())),
        }
    }

    /// The text of the symlink `name`.
    fn link_text(&self, name: &OsStr) -> Result<std::ffi::OsString> {
        sys(readlinkat(&self.fd, name)).ctx(|| format!("read symlink {}", self.entry(name).display()))
    }

    /// Hash (streamed) and metadata of the regular file `name`.
    fn observe_file(&self, name: &OsStr) -> Result<Observed> {
        let (mut file, meta) = self.open_regular(name)?;
        let hash = Hash::copy(&mut file, meta.len(), &mut io::sink())
            .ctx(|| format!("read {}", self.entry(name).display()))?;
        Ok(Observed::File {
            hash,
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
        })
    }

    /// Opens the regular file `name` for reading; fails on a symlink or a
    /// special file. `O_NONBLOCK` keeps a FIFO from blocking the open.
    fn open_regular(&self, name: &OsStr) -> Result<(File, fs::Metadata)> {
        let path = self.entry(name);
        let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
        let fd = sys(openat(&self.fd, name, flags, Mode::empty()))
            .ctx(|| format!("open {} (symlinks are not followed)", path.display()))?;
        let file = File::from(fd);
        let meta = file.metadata().ctx(|| format!("stat {}", path.display()))?;
        if !meta.is_file() {
            return Err(Error::Failed(format!("{} is not a regular file", path.display())));
        }
        Ok((file, meta))
    }

    /// Writes `want.1` bytes from `from` to `name` through a new temporary
    /// file in this directory: private (0600) from creation, streamed
    /// (`Hash::copy`), then owner and mode of `meta`, and fsync, then
    /// renamed to `name`. Content with another hash than `want.0` fails
    /// with "content changed while rustow was running", and short content
    /// fails too: the temporary file goes, `name` stays as it was.
    /// `replace` replaces a file or symlink at `name` (never a directory);
    /// otherwise anything there fails it.
    pub fn write(
        &self,
        name: &OsStr,
        from: &mut dyn Read,
        want: (&Hash, u64),
        meta: FileMeta,
        replace: bool,
    ) -> Result<()> {
        let dest = self.entry(name);
        let temp = temp_name();
        let flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let fd = sys(openat(&self.fd, temp.as_str(), flags, Mode::from_bits_truncate(0o600)))
            .ctx(|| format!("write {}", dest.display()))?;
        let done = fill(File::from(fd), &dest, from, want, meta).and_then(|()| {
            self.publish(OsStr::new(&temp), name, replace)
                .ctx(|| format!("replace {}", dest.display()))
        });
        if done.is_err() {
            let _ = unlinkat(&self.fd, temp.as_str(), UnlinkatFlags::NoRemoveDir);
        }
        done?;
        self.sync();
        Ok(())
    }

    fn publish(&self, temp: &OsStr, name: &OsStr, replace: bool) -> io::Result<()> {
        match replace {
            true => sys(renameat(&self.fd, temp, &self.fd, name)),
            false => rename_noreplace(&self.fd, temp, &self.fd, name),
        }
    }

    /// Creates the symlink `name` with `text`. `replace` replaces a file or
    /// symlink there atomically (never a directory); otherwise anything
    /// there fails it.
    pub fn symlink(&self, name: &OsStr, text: &Path, replace: bool) -> Result<()> {
        let made = match replace {
            false => sys(symlinkat(text, &self.fd, name)),
            true => {
                let temp = temp_name();
                let swapped = sys(symlinkat(text, &self.fd, temp.as_str()))
                    .and_then(|()| sys(renameat(&self.fd, temp.as_str(), &self.fd, name)));
                if swapped.is_err() {
                    let _ = unlinkat(&self.fd, temp.as_str(), UnlinkatFlags::NoRemoveDir);
                }
                swapped
            }
        };
        made.ctx(|| format!("create symlink {}", self.entry(name).display()))?;
        self.sync();
        Ok(())
    }

    /// Changes owner and mode of the regular file `name`; refuses a symlink.
    pub fn set_meta(&self, name: &OsStr, meta: FileMeta) -> Result<()> {
        let (file, _) = self.open_regular(name)?;
        chown_chmod(&file, &self.entry(name), (meta.uid, meta.gid), meta.mode)
    }

    /// Removes the file or symlink `name`, never a directory.
    pub fn remove(&self, name: &OsStr) -> Result<()> {
        sys(unlinkat(&self.fd, name, UnlinkatFlags::NoRemoveDir))
            .map_err(|e| Error::Failed(format!("remove {}: {e}", self.entry(name).display())))
    }

    /// Removes `name` when it is an empty directory; true when it did.
    pub fn rmdir(&self, name: &OsStr) -> bool {
        unlinkat(&self.fd, name, UnlinkatFlags::RemoveDir).is_ok()
    }

    /// The directory `name`, created with exactly `mode` when it is missing
    /// (true in the result): `fchmod` after `mkdirat`, so the umask and an
    /// inherited setgid bit do not count. An existing one must be a real
    /// directory that `check_dir` accepts for `me`, such as one an earlier
    /// item created.
    pub fn mkdir(&self, name: &OsStr, mode: u32, me: u32) -> Result<(Dir, bool)> {
        let path = self.entry(name);
        if Path::new(name).components().ne([Component::Normal(name)]) {
            return Err(Error::Failed(format!("{}: not a directory name", path.display())));
        }
        let bits = Mode::from_bits_truncate(mode as nix::libc::mode_t);
        if !self.create(name, bits)? {
            return self.trusted_child(name, me).map(|dir| (dir, false));
        }
        let what = || format!("create directory {}", path.display());
        let dir = sys(self.child(name, OFlag::O_RDONLY)).ctx(what)?;
        sys(fchmod(&dir.fd, bits)).ctx(what)?;
        Ok((dir, true))
    }

    /// `mkdirat`; false when `name` exists already.
    fn create(&self, name: &OsStr, bits: Mode) -> Result<bool> {
        match mkdirat(&self.fd, name, bits) {
            Ok(()) => Ok(true),
            Err(Errno::EEXIST) => Ok(false),
            Err(e) => Err(io::Error::from(e)).ctx(|| format!("create directory {}", self.entry(name).display())),
        }
    }

    /// The existing directory `name`, never a symlink, when `check_dir`
    /// accepts it for `me`.
    fn trusted_child(&self, name: &OsStr, me: u32) -> Result<Dir> {
        let path = self.entry(name);
        let dir = sys(self.child(name, SEARCH)).ctx(|| format!("create directory {}", path.display()))?;
        let st = sys(fstat(&dir.fd)).ctx(|| format!("inspect {}", path.display()))?;
        trusted(&path, &st, st.st_mode as u32 & 0o7777, me).map_err(Error::Failed)?;
        Ok(dir)
    }

    /// The private directory `name`: created with mode 0700, or an existing
    /// real directory that `me` owns. Only its owner bits stay, so a new
    /// one is exactly 0700 under a setgid parent too.
    pub(super) fn private(&self, name: &OsStr, me: u32) -> Result<Dir> {
        let path = self.entry(name);
        self.create(name, Mode::from_bits_truncate(0o700))?;
        let dir = sys(self.child(name, OFlag::O_RDONLY))
            .ctx(|| format!("open {} (symlinks are not followed)", path.display()))?;
        let st = sys(fstat(&dir.fd)).ctx(|| format!("inspect {}", path.display()))?;
        if st.st_uid != me {
            return Err(Error::Failed(format!(
                "{} is owned by uid {}, not by the current user (uid {me})",
                path.display(),
                st.st_uid
            )));
        }
        // Group and other access, setgid (inherited from a parent) and the
        // other special bits go; the owner bits stay as they are.
        let owner = st.st_mode & 0o700;
        if st.st_mode & 0o7777 != owner {
            sys(fchmod(&dir.fd, Mode::from_bits_truncate(owner))).ctx(|| format!("restrict {}", path.display()))?;
        }
        Ok(dir)
    }

    /// Moves the file or symlink `name` to `to_name` in `to`, never over
    /// anything there. Across filesystems it copies, then removes `name`.
    pub fn move_to(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> Result<()> {
        let (from, dest) = (self.entry(name), to.entry(to_name));
        let what = || format!("back up {} to {}", from.display(), dest.display());
        match rename_noreplace(&self.fd, name, &to.fd, to_name) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(Error::Failed(format!("backup {} already exists", dest.display())));
            }
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => self
                .copy_to(name, to, to_name)
                .and_then(|()| sys(unlinkat(&self.fd, name, UnlinkatFlags::NoRemoveDir)))
                .ctx(what)?,
            other => other.ctx(what)?,
        }
        to.sync();
        self.sync();
        Ok(())
    }

    /// Copies the file or symlink `name` to the new entry `to_name` in `to`.
    fn copy_to(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()> {
        let st = sys(fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW))?;
        if kind(&st) == SFlag::S_IFLNK {
            return sys(symlinkat(sys(readlinkat(&self.fd, name))?.as_os_str(), &to.fd, to_name));
        }
        let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
        let src = File::from(sys(openat(&self.fd, name, flags, Mode::empty()))?);
        let flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let mut dst = File::from(sys(openat(&to.fd, to_name, flags, Mode::from_bits_truncate(0o600)))?);
        io::copy(&mut &src, &mut dst)?;
        dst.set_permissions(src.metadata()?.permissions())?;
        dst.sync_all()
    }

    /// Best effort: makes a change of entries durable. Failure only weakens crash safety.
    fn sync(&self) {
        if let Ok(fd) = openat(&self.fd, ".", OFlag::O_RDONLY | OFlag::O_CLOEXEC, Mode::empty()) {
            let _ = File::from(fd).sync_all();
        }
    }
}

/// The walk of `Dir::walk`, with the symlink budget shared by nested walks.
fn walk_in(mut cur: Dir, rel: &Path, me: u32, budget: &mut u32) -> Result<Walked> {
    if rel.has_root() {
        cur = Dir::root()?;
    }
    // Names and `..`: a root or a `.` can only come first.
    let parts: Vec<&OsStr> = rel
        .components()
        .filter(|c| matches!(c, Component::Normal(_) | Component::ParentDir))
        .map(|c| c.as_os_str())
        .collect();
    for (i, name) in parts.iter().enumerate() {
        match cur.step(name, me, budget)? {
            Next::Into(next) => cur = next,
            Next::Missing => return Ok(missing(cur, &parts[i..])),
            Next::Blocked(path, reason) => return Ok(Walked::Blocked { path, reason }),
        }
    }
    Ok(Walked::Ready(cur))
}

fn missing(dir: Dir, rest: &[&OsStr]) -> Walked {
    let missing = rest.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    Walked::Missing { dir, missing }
}

/// Streams the content, checks its hash, sets owner and mode, and syncs
/// the temporary file of `dest`.
fn fill(mut file: File, dest: &Path, from: &mut dyn Read, (hash, len): (&Hash, u64), meta: FileMeta) -> Result<()> {
    let written = Hash::copy(from, len, &mut file).ctx(|| format!("write {}", dest.display()))?;
    if written != *hash {
        return Err(Error::Failed(format!(
            "{}: content changed while rustow was running; left unchanged",
            dest.display()
        )));
    }
    chown_chmod(&file, dest, (meta.uid, meta.gid), meta.mode)?;
    file.sync_all().ctx(|| format!("write {}", dest.display()))
}

/// Sets the owner and group, then the mode, of the open file of `path`.
fn chown_chmod(file: &File, path: &Path, (uid, gid): (Option<u32>, Option<u32>), mode: Option<u32>) -> Result<()> {
    if uid.is_some() || gid.is_some() {
        std::os::unix::fs::fchown(file, uid, gid).ctx(|| format!("set owner of {}", path.display()))?;
    }
    if let Some(mode) = mode {
        file.set_permissions(Permissions::from_mode(mode))
            .ctx(|| format!("set mode of {}", path.display()))?;
    }
    Ok(())
}

/// Renames without replacing anything at `to`. Where the kernel or the
/// filesystem has no such rename, a hard link (which fails when `to`
/// exists) and an unlink do the same.
fn rename_noreplace(from_dir: &OwnedFd, from: &OsStr, to_dir: &OwnedFd, to: &OsStr) -> io::Result<()> {
    use rustix::fs::{RenameFlags, renameat_with};
    use rustix::io::Errno as E;
    match renameat_with(from_dir, from, to_dir, to, RenameFlags::NOREPLACE) {
        Err(E::INVAL | E::NOSYS) => {
            sys(linkat(from_dir, from, to_dir, to, AtFlags::empty()))?;
            let _ = unlinkat(from_dir, from, UnlinkatFlags::NoRemoveDir);
            Ok(())
        }
        other => other.map_err(io::Error::from),
    }
}

/// A new name for a temporary file: `TEMP_PREFIX`, the process id, the
/// time and a counter, so two calls never give the same name.
fn temp_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{TEMP_PREFIX}{}-{nanos:x}-{n}", std::process::id())
}

fn sys<T>(r: nix::Result<T>) -> io::Result<T> {
    r.map_err(io::Error::from)
}

/// `check_dir` for `me` on what `st` describes, with `mode` as its permission bits.
fn trusted(path: &Path, st: &FileStat, mode: u32, me: u32) -> std::result::Result<(), String> {
    let owned = Owned {
        path: path.to_path_buf(),
        uid: st.st_uid,
        gid: st.st_gid,
        mode,
    };
    check_dir(&owned, me, private_gid_of(me))
}

fn kind(st: &FileStat) -> SFlag {
    SFlag::from_bits_truncate(st.st_mode) & SFlag::S_IFMT
}

fn id_of(st: &FileStat) -> DirId {
    DirId {
        dev: st.st_dev,
        ino: st.st_ino,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counter alone keeps two names apart, also when the clock does not move.
    #[test]
    fn temp_names_are_reserved_and_count_up() {
        let (a, b) = (temp_name(), temp_name());
        assert!(a.starts_with(TEMP_PREFIX) && b.starts_with(TEMP_PREFIX), "{a} {b}");
        let count = |n: &str| n.rsplit('-').next().unwrap().parse::<u64>().unwrap();
        assert!(count(&b) > count(&a), "{a} {b}");
    }
}
