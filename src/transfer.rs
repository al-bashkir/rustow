//! `rustow state export|import`: the registrations of `file add` and
//! copy-mode `link` as JSON, to move them to another machine. The records
//! of deployed files stay: they belong to one machine. `rustow state
//! adopt`: the whole state of this repository from its path before a move.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use nix::fcntl::OFlag;
use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::fsops;
use crate::hash::Hash;
use crate::paths::RelPath;
use crate::repo::Context;
use crate::report::{Change, Printer};
use crate::state::{Lists, MAX_STATE_SIZE, Registration, State};

/// Version of the export format; independent of the state version.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registrations {
    version: u32,
    #[serde(default)]
    files: BTreeSet<RelPath>,
    #[serde(default)]
    links: BTreeSet<RelPath>,
}

/// Prints the registrations as JSON, or writes them to `file`: a new file,
/// or any file when `replace` (`--force`). "-" prints them, as `import`
/// reads "-" from stdin.
pub fn export(ctx: &Context, file: Option<&Path>, replace: bool) -> Result<i32> {
    let file = file.filter(|f| *f != Path::new("-"));
    let state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let Lists { files, links } = Lists::of(&state.registrations);
    let regs = Registrations {
        version: FORMAT,
        files,
        links,
    };
    let mut text =
        serde_json::to_string_pretty(&regs).map_err(|e| Error::Failed(format!("encode registrations: {e}")))?;
    text.push('\n');
    match file {
        Some(path) if replace => fsops::write_atomic(path, text.as_bytes(), 0o644, None, None)?,
        Some(path) if fs::symlink_metadata(path).is_ok() => {
            return Err(Error::Usage(format!(
                "{} already exists; add --force to replace it",
                path.display()
            )));
        }
        Some(path) => fsops::write_new(path, text.as_bytes(), 0o644)?,
        None => crate::report::out(format_args!("{text}")),
    }
    Ok(0)
}

/// Adds the registrations in `file` ("-" is stdin) to the state of this
/// machine. Registrations that are there already stay; an imported copy
/// replaces a link registration. Each path must pass the check of `file
/// add` or of `link <path>`; else nothing changes.
pub fn import(ctx: &mut Context, printer: &Printer, file: &Path, dry_run: bool) -> Result<i32> {
    let mut regs = read(file)?;
    // A path in both lists is a copy, as in the state file.
    regs.links.retain(|l| !regs.files.contains(l));
    let mut state = ctx.load_state()?;
    check_paths(ctx, &state, &regs)?;
    let changes = add(&mut state, regs);
    if !dry_run && !changes.is_empty() {
        state.save(&ctx.state_dir, &ctx.repo_id)?;
    }
    printer.changes(None, &changes, dry_run);
    Ok(0)
}

/// Adds each registration that `state` does not hold yet; one change for each.
fn add(state: &mut State, regs: Registrations) -> Vec<Change> {
    let mut changes = Vec::new();
    for (list, kind, label) in [
        (regs.files, Registration::Copy, "copy"),
        (regs.links, Registration::Link, "link"),
    ] {
        for path in list {
            if state.registrations.insert(path.clone(), kind) != Some(kind) {
                changes.push(Change::new(path.as_str(), "import", Some(label.into())));
            }
        }
    }
    changes
}

/// Checks each `files` path as `file add` does and each `links` path as
/// `link <path>` does.
fn check_paths(ctx: &Context, state: &State, regs: &Registrations) -> Result<()> {
    let files: Vec<String> = regs.files.iter().map(ToString::to_string).collect();
    crate::files::check_copies(ctx, &files)?;
    let discovery = ctx.discover()?;
    regs.links
        .iter()
        .try_for_each(|link| crate::app::check_link(ctx, &discovery, state, link.as_str()))
}

/// Takes over the state of the repository that was at `old` before a move:
/// sets the root, moves its cache entries to this repository id, saves it
/// as this repository's state (`take_over`). `apply` then updates the links.
pub fn adopt(ctx: &Context, printer: &Printer, old: &Path, dry_run: bool) -> Result<i32> {
    let (old, old_id) = adoptable(ctx, old)?;
    let mut state = State::load(&ctx.state_dir, &old_id, &old)?;
    state.repo = ctx.root.clone();
    for record in state.entries.values_mut() {
        let moved = |p: PathBuf| moved_cache_path(ctx, &p, &old_id).unwrap_or(p);
        record.cache_path = record.cache_path.take().map(moved);
    }
    if !dry_run {
        take_over(ctx, &state, &old_id)?;
    }
    let detail = crate::report::plural(state.entries.len(), "record");
    printer.changes(
        None,
        &[Change::new(&old.display().to_string(), "adopt", Some(detail))],
        dry_run,
    );
    Ok(0)
}

/// Writes the adopted `state`: moves the cache entries of `old_id`, saves
/// the state of this repository, keeps the old file as
/// `backups/<old-id>/state-adopted.json` and removes the old lock file,
/// unless a run still holds it.
fn take_over(ctx: &Context, state: &State, old_id: &str) -> Result<()> {
    move_cache(ctx, old_id)?;
    state.save(&ctx.state_dir, &ctx.repo_id)?;
    let (file, backups) = (
        State::file(&ctx.state_dir, old_id),
        ctx.state_dir.join("backups").join(old_id),
    );
    fsops::ensure_private_dir(&backups)?;
    let kept = backups.join("state-adopted.json");
    fs::rename(&file, &kept).ctx(|| format!("move {} to {}", file.display(), kept.display()))?;
    if let Ok(lock) = crate::state::lock(&ctx.state_dir, old_id) {
        fs::remove_file(&lock.path).ctx(|| format!("remove {}", lock.path.display()))?;
    }
    Ok(())
}

/// `old` in the form that the state records, and its repository id, when
/// `adopt` may take its state over: `old` does not exist, its state file
/// does, and this repository has none.
fn adoptable(ctx: &Context, old: &Path) -> Result<(PathBuf, String)> {
    let bad = |why: String| Error::Usage(format!("{}: {why}", old.display()));
    let old: PathBuf = std::path::absolute(old)
        .map_err(|e| bad(e.to_string()))?
        .components()
        .collect();
    if fs::symlink_metadata(&old).is_ok() {
        return Err(bad(
            "still exists; rustow adopts only the state of a repository that was moved".into(),
        ));
    }
    let old_id = Hash::of(old.as_os_str().as_encoded_bytes()).short().to_owned();
    if fs::symlink_metadata(State::file(&ctx.state_dir, &old_id)).is_err() {
        let dir = ctx.state_dir.display();
        return Err(Error::Usage(format!(
            "no state of a repository at {} in {dir}",
            old.display()
        )));
    }
    let ours = State::file(&ctx.state_dir, &ctx.repo_id);
    if fs::symlink_metadata(&ours).is_ok() {
        return Err(Error::Usage(format!(
            "this repository already has a state ({}); move it away first",
            ours.display()
        )));
    }
    Ok((old, old_id))
}

/// `path` of a cache entry below `<kind>/<old_id>` in the old cache, below
/// `<kind>/<repo-id>` in this repository's cache.
fn moved_cache_path(ctx: &Context, path: &Path, old_id: &str) -> Option<PathBuf> {
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    let kind = |p: &std::ffi::OsStr| p == "rendered" || p == "decrypted";
    let at = parts.windows(2).position(|w| kind(w[0]) && w[1] == old_id)?;
    let mut new = ctx.cache_dir.join(parts[at]).join(&ctx.repo_id);
    new.extend(&parts[at + 2..]);
    Some(new)
}

/// Renames the cache entries of `old_id` to this repository id, in each
/// cache kind that has them.
fn move_cache(ctx: &Context, old_id: &str) -> Result<()> {
    for kind in ["rendered", "decrypted"] {
        let base = ctx.cache_dir.join(kind);
        if !fs::symlink_metadata(base.join(old_id)).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let dir = fsops::private_tree(&ctx.cache_dir, &base)?;
        let (from, to) = (dir.path.join(old_id), dir.path.join(&ctx.repo_id));
        fs::rename(&from, &to).ctx(|| format!("move {} to {}", from.display(), to.display()))?;
    }
    Ok(())
}

/// Reads and checks an export. Each path is checked as a repository path.
fn read(file: &Path) -> Result<Registrations> {
    let bad = |why: String| Error::Usage(format!("{}: {why}", file.display()));
    let bytes = match file == Path::new("-") {
        true => fsops::read_at_most(std::io::stdin(), "standard input", MAX_STATE_SIZE),
        false => fsops::read_limited(file, OFlag::empty(), MAX_STATE_SIZE),
    }
    .map_err(|e| Error::Usage(e.to_string()))?;
    let regs: Registrations = serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?;
    if regs.version != FORMAT {
        return Err(bad(format!(
            "unsupported registrations version {} (this rustow reads {FORMAT})",
            regs.version
        )));
    }
    Ok(regs)
}
