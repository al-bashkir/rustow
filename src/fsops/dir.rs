//! Directories held open. rustow resolves a destination's directory one
//! component at a time and keeps it open, so that a path changed after
//! the check cannot send a later change somewhere else.

use std::ffi::OsStr;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{AT_FDCWD, AtFlags, OFlag, openat, readlinkat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstat, fstatat};
use serde::{Deserialize, Serialize};

use super::{Owned, check_dir, private_gid_of};
use crate::error::{IoContext, Result};

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

impl Dir {
    /// Walks the absolute path `abs` from `/` (see `walk`).
    pub fn open(abs: &Path, me: u32) -> Result<Walked> {
        Dir::root()?.walk(abs, me)
    }

    fn root() -> Result<Dir> {
        let fd = openat(
            AT_FDCWD,
            "/",
            SEARCH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .ctx(|| "open /".to_owned())?;
        Ok(Dir { fd, path: "/".into() })
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
        let st = fstat(&self.fd)
            .map_err(std::io::Error::from)
            .ctx(|| format!("inspect {}", self.path.display()))?;
        Ok(id_of(&st))
    }

    /// One component: `..`, a directory, a missing entry or a symlink.
    fn step(&self, name: &OsStr, me: u32, budget: &mut u32) -> Result<Next> {
        let path = match name == ".." {
            true => self.path.parent().unwrap_or(&self.path).to_path_buf(),
            false => self.path.join(name),
        };
        let flags = SEARCH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        match openat(&self.fd, name, flags, Mode::empty()) {
            Ok(fd) => Ok(Next::Into(Dir { fd, path })),
            Err(Errno::ENOENT) => Ok(Next::Missing),
            Err(Errno::ELOOP | Errno::ENOTDIR) => self.follow(name, path, me, budget),
            Err(e) => Ok(Next::Blocked(path, e.desc().into())),
        }
    }

    /// Follows the symlink `name` (at `path`) if the trust rule allows it.
    fn follow(&self, name: &OsStr, path: PathBuf, me: u32, budget: &mut u32) -> Result<Next> {
        let link = match fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(st) if kind(&st) == SFlag::S_IFLNK => st,
            Ok(_) => return Ok(Next::Blocked(path, "not a directory".into())),
            Err(e) => return Ok(Next::Blocked(path, e.desc().into())),
        };
        // A symlink's own mode bits mean nothing: only its owner counts.
        if let Err(why) = trusted(&path, &link, 0, me) {
            return Ok(Next::Blocked(path, format!("symlink is not safe: {why}")));
        }
        if *budget == 0 {
            return Ok(Next::Blocked(path, Errno::ELOOP.desc().into()));
        }
        *budget -= 1;
        let target = match self.reach(name, &path, me, budget)? {
            Ok(dir) => dir,
            Err(blocked) => return Ok(blocked),
        };
        let st = fstat(&target.fd)
            .map_err(std::io::Error::from)
            .ctx(|| format!("inspect {}", target.path.display()))?;
        if let Err(why) = trusted(&target.path, &st, st.st_mode as u32 & 0o7777, me) {
            return Ok(Next::Blocked(path, format!("symlink target is not safe: {why}")));
        }
        match fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(again) if id_of(&again) == id_of(&link) => Ok(Next::Into(target)),
            _ => Ok(Next::Blocked(path, "changed while rustow was running".into())),
        }
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
}

/// The walk of `Dir::walk`, with the symlink budget shared by nested walks.
fn walk_in(mut cur: Dir, rel: &Path, me: u32, budget: &mut u32) -> Result<Walked> {
    let parts: Vec<Component> = rel.components().collect();
    for (i, part) in parts.iter().enumerate() {
        let name = match part {
            Component::RootDir => {
                cur = Dir::root()?;
                continue;
            }
            Component::CurDir | Component::Prefix(_) => continue,
            Component::ParentDir => OsStr::new(".."),
            Component::Normal(name) => name,
        };
        cur = match cur.step(name, me, budget)? {
            Next::Into(next) => next,
            Next::Missing => {
                let missing = parts[i..]
                    .iter()
                    .map(|p| p.as_os_str().to_string_lossy().into_owned())
                    .collect();
                return Ok(Walked::Missing { dir: cur, missing });
            }
            Next::Blocked(path, reason) => return Ok(Walked::Blocked { path, reason }),
        };
    }
    Ok(Walked::Ready(cur))
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
