//! `rustow init`: prepares a new or existing repository. Never deploys,
//! never runs Git, and never replaces a file it did not expect.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::config::edit;
use crate::config::{CONFIG_FILE, Config};
use crate::error::{Error, IoContext, Result};
use crate::fsops;
use crate::ignore::{DEFAULT_CACHE_DIR, IGNORE_FILE};
use crate::paths::RelPath;
use crate::repo::{self, Env, GlobalOpts, InternalDirs};
use crate::report::Change;

#[derive(Debug)]
pub struct InitReport {
    pub root: PathBuf,
    pub notices: Vec<String>,
    pub changes: Vec<Change>,
}

pub fn init(opts: &GlobalOpts, env: &Env, dry_run: bool) -> Result<InitReport> {
    let mut notices = Vec::new();
    let root = repo::find_root(opts, env, &mut notices)?;
    if root.parent().is_none() {
        return Err(Error::Usage("refusing to initialize the filesystem root".into()));
    }
    let mut changes = Vec::new();
    let config = init_config(&root, dry_run, &mut changes)?;
    let dirs = InternalDirs::resolve(&root, &env.home, &config, env)?;
    let mut wanted = vec![RelPath::parse(DEFAULT_CACHE_DIR)?];
    wanted.extend(dirs.inside.iter().filter(|d| d.as_str() != DEFAULT_CACHE_DIR).cloned());
    for name in [".gitignore", IGNORE_FILE] {
        init_ignore_file(&root, name, &wanted, dry_run, &mut changes)?;
    }
    init_cache_dir(&root, &dirs.cache_dir, dry_run, &mut changes)?;
    Ok(InitReport { root, notices, changes })
}

fn init_config(root: &Path, dry_run: bool, changes: &mut Vec<Change>) -> Result<Config> {
    let path = root.join(CONFIG_FILE);
    let (text, change) = match read_text(&path)? {
        None => {
            let text = edit::minimal();
            write(&path, &text, None, dry_run)?;
            (text, Change::new(CONFIG_FILE, "create", None))
        }
        Some((text, mode)) => match edit::ensure_init(&text)? {
            Some(new) => {
                write(&path, &new, Some(mode), dry_run)?;
                (
                    new,
                    Change::new(CONFIG_FILE, "update", Some("added version = 1".into())),
                )
            }
            None => (text, Change::new(CONFIG_FILE, "ok", None)),
        },
    };
    changes.push(change);
    Config::parse(&text)
}

fn init_ignore_file(root: &Path, name: &str, dirs: &[RelPath], dry_run: bool, changes: &mut Vec<Change>) -> Result<()> {
    let path = root.join(name);
    let existing = read_text(&path)?;
    let mut text = existing.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
    let mut added = Vec::new();
    for dir in dirs {
        if let Some(updated) = edit::ensure_ignore_entry(&text, dir) {
            text = updated;
            added.push(format!("/{dir}/"));
        }
    }
    if added.is_empty() {
        changes.push(Change::new(name, "ok", None));
        return Ok(());
    }
    write(&path, &text, existing.as_ref().map(|(_, m)| *m), dry_run)?;
    let action = if existing.is_some() { "update" } else { "create" };
    changes.push(Change::new(name, action, Some(format!("added {}", added.join(" ")))));
    Ok(())
}

fn init_cache_dir(root: &Path, dir: &Path, dry_run: bool, changes: &mut Vec<Change>) -> Result<()> {
    let name = dir
        .strip_prefix(root)
        .map_or_else(|_| dir.display().to_string(), |r| format!("{}/", r.display()));
    let action = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => "create",
        Err(e) => return Err(e).ctx(|| format!("inspect {}", dir.display())),
        Ok(m) if m.is_dir() && m.mode() & 0o077 != 0 => "update",
        Ok(m) if m.is_dir() => "ok",
        Ok(_) => {
            return Err(Error::Conflict(format!(
                "{} exists and is not a directory (symlinks are not followed); move it away first",
                dir.display()
            )));
        }
    };
    if !dry_run {
        fsops::ensure_private_dir(dir)?;
    }
    let detail = (action != "ok").then(|| "mode 0700".to_owned());
    changes.push(Change::new(&name, action, detail));
    Ok(())
}

/// Text and mode of an existing regular file; `None` when absent. Any
/// other object (symlink, directory) is a conflict that init will not touch.
fn read_text(path: &Path) -> Result<Option<(String, u32)>> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).ctx(|| format!("inspect {}", path.display())),
        Ok(m) => m,
    };
    if !meta.is_file() {
        return Err(Error::Conflict(format!(
            "{} is not a regular file (symlinks are not followed); rustow init will not replace it",
            path.display()
        )));
    }
    let text = String::from_utf8(fsops::read_file(path)?)
        .map_err(|_| Error::Config(format!("{} is not valid UTF-8", path.display())))?;
    Ok(Some((text, meta.permissions().mode() & 0o7777)))
}

fn write(path: &Path, text: &str, mode: Option<u32>, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    fsops::write_atomic(path, text.as_bytes(), mode.unwrap_or(0o644), None, None)
}
