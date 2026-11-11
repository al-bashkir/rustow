//! Repository-local cache of rendered and decrypted content.
//!
//! Layout: `<cache>/<rendered|decrypted>/<repo-id>/<scope-id>/<dest>`.
//! The repository id separates repositories that share a configured
//! cache; the scope id separates target roots; the destination path
//! separates sources (two sources never share a destination).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};
use crate::repo::Context;
use crate::report::Change;
use crate::source::{Mode, Source};
use crate::state::State;

/// Cache entry for a source, or `None` for modes that use no cache.
pub fn entry(ctx: &Context, source: &Source) -> Option<PathBuf> {
    let kind = match source.mode {
        Mode::Link | Mode::FileCopy => return None,
        Mode::TemplateLink(_) | Mode::TemplateCopy(..) => "rendered",
        Mode::DecryptLink(_) | Mode::DecryptCopy(_) => "decrypted",
    };
    let dir = ctx
        .cache_dir
        .join(kind)
        .join(&ctx.repo_id)
        .join(ctx.scope_id(source.scope));
    Some(dir.join(source.dest.as_path()))
}

/// Removes this repository's rendered/decrypted entries that no managed
/// symlink uses (entries behind copies are regenerated on demand), then
/// empty directories. Never follows symlinks.
pub fn clean(ctx: &Context, state: &State, dry_run: bool) -> Result<Vec<Change>> {
    if !cache_root_present(&ctx.cache_dir)? {
        return Ok(Vec::new());
    }
    let live: BTreeSet<&Path> = state
        .entries
        .values()
        .filter(|r| r.link_target.is_some())
        .filter_map(|r| r.cache_path.as_deref())
        .collect();
    let mut changes = Vec::new();
    for kind in ["rendered", "decrypted"] {
        changes.extend(clean_kind(ctx, kind, &live, dry_run)?);
    }
    changes.extend(clean_workspaces(&ctx.cache_dir.join("edit"), dry_run)?);
    Ok(changes)
}

/// False when there is no cache directory; an error when it is not a real directory.
fn cache_root_present(cache_dir: &Path) -> Result<bool> {
    match fs::symlink_metadata(cache_dir) {
        Err(_) => Ok(false),
        Ok(m) if !m.is_dir() => {
            let shown = cache_dir.display();
            Err(Error::Conflict(format!(
                "{shown} is not a directory (symlinks are not followed)"
            )))
        }
        Ok(_) => Ok(true),
    }
}

/// Walks this repository's entries of one cache kind and removes the unused ones.
fn clean_kind(ctx: &Context, kind: &str, live: &BTreeSet<&Path>, dry_run: bool) -> Result<Vec<Change>> {
    let base = ctx.cache_dir.join(kind).join(&ctx.repo_id);
    let mut changes = Vec::new();
    if !fs::symlink_metadata(&base).is_ok_and(|m| m.is_dir()) {
        return Ok(changes);
    }
    for entry in walkdir::WalkDir::new(&base).follow_links(false).contents_first(true) {
        let entry = entry.map_err(|e| Error::Failed(format!("scan {}: {e}", base.display())))?;
        let path = entry.path();
        if entry.file_type().is_dir() {
            if path != base && !dry_run {
                // Fails, harmlessly, while the directory still has entries.
                let _ = fs::remove_dir(path);
            }
        } else if let Some(change) = clean_file(ctx, path, live, dry_run)? {
            changes.push(change);
        }
    }
    Ok(changes)
}

/// Removes one cache file unless a managed symlink uses it.
fn clean_file(ctx: &Context, path: &Path, live: &BTreeSet<&Path>, dry_run: bool) -> Result<Option<Change>> {
    if live.contains(path) {
        return Ok(None);
    }
    if !dry_run {
        fs::remove_file(path).ctx(|| format!("remove {}", path.display()))?;
    }
    let shown = path.strip_prefix(&ctx.cache_dir).unwrap_or(path).display().to_string();
    Ok(Some(Change::new(&shown, "remove", None)))
}

/// Removes `enc edit` workspaces whose lock no running edit holds.
fn clean_workspaces(root: &Path, dry_run: bool) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    if !fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        return Ok(changes);
    }
    for entry in fs::read_dir(root).ctx(|| format!("read {}", root.display()))?.flatten() {
        let path = entry.path();
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let in_use = fs::File::open(path.join(".lock")).is_ok_and(|f| f.try_lock().is_err());
        if in_use {
            continue;
        }
        if !dry_run {
            fs::remove_dir_all(&path).ctx(|| format!("remove {}", path.display()))?;
        }
        let name = format!("edit/{}", entry.file_name().to_string_lossy());
        changes.push(Change::new(
            &name,
            "remove",
            Some("stale edit workspace (may hold plaintext)".into()),
        ));
    }
    Ok(changes)
}
