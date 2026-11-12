//! Directory folding (spec §7.1). A home directory whose repository copy
//! holds only ordinary link sources that may fold (`fold_allowed`), and
//! that does not exist at the destination yet, is deployed as one symlink.
//! A direct child of home never folds: on a fresh machine `~/.ssh`,
//! `~/.gnupg` or `~/.config` would then take private files into the repo.
//! A folded link that no longer qualifies is unfolded into a real
//! directory, but only when every file it exposes is a source and the same
//! run deploys every one of them again (see `plan`), so that no content
//! seen through the link disappears from view.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::error::{Error, Result};
use crate::fsops::{self, ParentState};
use crate::paths::{RelPath, Scope};
use crate::repo::Context;
use crate::source::{Discovery, Mode, Source};
use crate::state::{Record, State};

#[derive(Debug, Default)]
pub struct Folding {
    /// Directories deployed (or kept) as one symlink.
    pub folds: Vec<RelPath>,
    /// Recorded folded links to replace by real directories.
    pub unfolds: Vec<RelPath>,
}

impl Folding {
    /// True when a fold already deploys this source.
    pub fn covered(&self, source: &Source) -> bool {
        source.scope == Scope::Home && self.folds.iter().any(|f| source.path.strip_prefix(f).is_some())
    }

    /// The unfolding directory this source lies below.
    pub fn unfolding(&self, source: &Source) -> Option<&RelPath> {
        if source.scope != Scope::Home {
            return None;
        }
        self.unfolds.iter().find(|u| source.dest.strip_prefix(u).is_some())
    }
}

/// Destinations involved in a collision, with the collision text (see `plan`).
pub type Collided<'a> = BTreeMap<(Scope, &'a RelPath), String>;

pub fn analyze(
    ctx: &Context,
    discovery: &Discovery,
    selected: &[&Source],
    state: &State,
    collided: &Collided,
) -> Result<Folding> {
    // A recorded fold whose link was replaced is stale: what is there now
    // belongs to the user, and files below it are planned against it.
    let recorded: BTreeMap<RelPath, &Record> = recorded_folds(ctx, state)
        .into_iter()
        .filter(|(dir, r)| still_linked(ctx, dir, r))
        .collect();
    let folds = fold_candidates(ctx, discovery, selected, &recorded, collided)?;
    let unfolds = select_unfolds(selected, &recorded, &folds);
    Ok(Folding { folds, unfolds })
}

fn is_home(source: &Source) -> bool {
    source.scope == Scope::Home
}

/// The `[[file_rules]]` value of `path` or of its nearest ancestor
/// directory; `default` when no rule covers it.
pub fn fold_allowed(rules: &BTreeMap<RelPath, bool>, default: bool, path: &RelPath) -> bool {
    std::iter::once(path.clone())
        .chain(ancestors(path))
        .find_map(|p| rules.get(&p).copied())
        .unwrap_or(default)
}

/// A home link source that may sit below a folded directory.
fn foldable(ctx: &Context, source: &Source) -> bool {
    is_home(source)
        && source.mode == Mode::Link
        && fold_allowed(
            &ctx.config.file_rules,
            ctx.config.defaults.fold_directories,
            &source.path,
        )
}

/// Directories above the selected link sources that can be one symlink; the outermost wins.
fn fold_candidates(
    ctx: &Context,
    discovery: &Discovery,
    selected: &[&Source],
    recorded: &BTreeMap<RelPath, &Record>,
    collided: &Collided,
) -> Result<Vec<RelPath>> {
    let link_sources: BTreeSet<&RelPath> = discovery
        .sources
        .iter()
        .filter(|s| foldable(ctx, s))
        .map(|s| &s.path)
        .collect();
    let chosen: BTreeSet<&RelPath> = selected.iter().filter(|s| foldable(ctx, s)).map(|s| &s.path).collect();
    let mut candidates: Vec<RelPath> = chosen
        .iter()
        .flat_map(|p| ancestors(p))
        .filter(|d| d.components().count() > 1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    candidates.sort_by_key(|d| (d.components().count(), d.clone()));
    let mut folds: Vec<RelPath> = Vec::new();
    for dir in candidates {
        if folds.iter().any(|f| dir.starts_with(f)) {
            continue;
        }
        // Keeping rustow's own link changes nothing; a new fold must not
        // expose sources that this run does not select.
        let record = recorded.get(&dir).copied();
        let allowed = if record.is_some() { &link_sources } else { &chosen };
        if qualifies(ctx, &dir, record, allowed, collided)? {
            folds.push(dir);
        }
    }
    Ok(folds)
}

/// `dir` can be one symlink: no home destination collides at or below it,
/// its destination allows a fold, and it holds only `allowed` sources.
fn qualifies(
    ctx: &Context,
    dir: &RelPath,
    record: Option<&Record>,
    allowed: &BTreeSet<&RelPath>,
    collided: &Collided,
) -> Result<bool> {
    let collides = collided
        .keys()
        .any(|(scope, dest)| *scope == Scope::Home && dest.starts_with(dir));
    Ok(!collides && dest_allows_fold(ctx, dir, record)? && holds_only(ctx, dir, allowed)?)
}

/// Recorded folded links that a selected source now lies below and that are no longer folded.
fn select_unfolds(selected: &[&Source], recorded: &BTreeMap<RelPath, &Record>, folds: &[RelPath]) -> Vec<RelPath> {
    recorded
        .keys()
        .filter(|dir| {
            selected
                .iter()
                .any(|s| is_home(s) && s.dest.strip_prefix(dir).is_some())
        })
        .filter(|dir| !folds.contains(dir))
        .cloned()
        .collect()
}

/// Repository files below `dir` that are not discovered sources: they
/// would vanish from the destination if the folded link were unfolded.
pub fn strays(ctx: &Context, discovery: &Discovery, dir: &RelPath) -> Result<Vec<RelPath>> {
    let sources: BTreeSet<&RelPath> = discovery.sources.iter().map(|s| &s.path).collect();
    Ok(entries_below(ctx, dir)?
        .into_iter()
        .filter(|(f, is_dir)| !is_dir && !sources.contains(f))
        .map(|(f, _)| f)
        .collect())
}

/// Folded-directory records of the current home target, by repository path.
fn recorded_folds<'a>(ctx: &Context, state: &'a State) -> BTreeMap<RelPath, &'a Record> {
    state
        .entries
        .iter()
        .filter(|(_, r)| r.folded_dir && r.scope == Scope::Home)
        .filter_map(|(dest, r)| {
            let rel = dest.strip_prefix(&ctx.home_target).ok()?.to_str()?;
            Some((RelPath::parse(rel).ok()?, r))
        })
        .collect()
}

fn ancestors(path: &RelPath) -> Vec<RelPath> {
    let mut out = Vec::new();
    let mut cur = path.parent();
    while let Some(p) = cur {
        cur = p.parent();
        out.push(p);
    }
    out
}

/// The destination of `dir` is still exactly the symlink rustow recorded.
fn still_linked(ctx: &Context, dir: &RelPath, record: &Record) -> bool {
    let text = fs::read_link(ctx.home_target.join(dir.as_path())).ok();
    text.is_some() && text == record.link_target
}

/// The destination of `dir` is missing (with usable parents) or is still
/// rustow's own folded link. `record` is one of the recorded folds that
/// `analyze` kept, so its link is still in place.
fn dest_allows_fold(ctx: &Context, dir: &RelPath, record: Option<&Record>) -> Result<bool> {
    match fs::symlink_metadata(ctx.home_target.join(dir.as_path())) {
        Err(_) => Ok(!matches!(
            fsops::parent_state(&ctx.home_target, dir)?,
            ParentState::Blocked { .. }
        )),
        Ok(m) if m.file_type().is_symlink() => Ok(record.is_some()),
        Ok(_) => Ok(false),
    }
}

/// No entry below `dir` is reserved (cache, state, `.git`), not even a
/// cache or state directory that does not exist yet, and every
/// non-directory entry is one of `allowed`: the fold would expose them.
fn holds_only(ctx: &Context, dir: &RelPath, allowed: &BTreeSet<&RelPath>) -> Result<bool> {
    if ctx.ignore.holds_internal(dir) {
        return Ok(false);
    }
    Ok(entries_below(ctx, dir)?
        .iter()
        .all(|(p, is_dir)| !ctx.ignore.is_reserved(p, *is_dir) && (*is_dir || allowed.contains(p))))
}

/// Repository-relative paths of all entries below `dir` (symlinks and
/// special files included) and whether each is a directory, without
/// following links.
fn entries_below(ctx: &Context, dir: &RelPath) -> Result<Vec<(RelPath, bool)>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(ctx.root.join(dir.as_path()))
        .min_depth(1)
        .follow_links(false)
    {
        let entry = entry.map_err(|e| Error::Failed(format!("scan {dir}: {e}")))?;
        let rel = entry.path().strip_prefix(&ctx.root).unwrap_or(entry.path());
        let rel = rel
            .to_str()
            .ok_or_else(|| Error::Config(format!("non-UTF-8 path below {dir}")))?;
        out.push((RelPath::parse(rel)?, entry.file_type().is_dir()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(pairs: &[(&str, bool)]) -> BTreeMap<RelPath, bool> {
        pairs.iter().map(|(p, v)| (RelPath::parse(p).unwrap(), *v)).collect()
    }

    fn rel(p: &str) -> RelPath {
        RelPath::parse(p).unwrap()
    }

    #[test]
    fn nearest_rule_wins_else_default() {
        let r = rules(&[
            (".config/nvim", false),
            (".config/nvim/lua", true),
            (".config/a/b.txt", false),
        ]);
        assert!(!fold_allowed(&r, true, &rel(".config/nvim/init.lua")));
        assert!(fold_allowed(&r, true, &rel(".config/nvim/lua/x.lua")));
        assert!(!fold_allowed(&r, true, &rel(".config/a/b.txt")));
        assert!(fold_allowed(&r, true, &rel(".config/a/c/b.txt")));
        assert!(!fold_allowed(&r, false, &rel(".config/other/x")));
    }
}
