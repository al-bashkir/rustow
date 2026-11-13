//! Directory folding (spec §7.1). A home directory whose repository copy
//! holds only ordinary link sources that may fold (`fold_allowed`), and
//! that does not exist at the destination yet, is deployed as one symlink.
//! A direct child of home or of `~/.local` never folds: on a fresh machine `~/.ssh`,
//! `~/.gnupg`, `~/.config` or `~/.local/share` would then take private files into the repo.
//! A folded link that no longer qualifies is unfolded into a real
//! directory, but only when every file it exposes is a source and the same
//! run deploys every one of them again (see `plan`), so that no content
//! seen through the link disappears from view.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;

use nix::unistd::geteuid;

use crate::error::Result;
use crate::fsops::{self, ParentState};
use crate::paths::{RelPath, Scope, same_link_target};
use crate::repo::Context;
use crate::source::{Discovery, Mode, Source};
use crate::state::{Record, State};

#[derive(Debug, Default)]
pub struct Folding {
    /// Directories deployed (or kept) as one symlink.
    pub folds: BTreeSet<RelPath>,
    /// Recorded folded links to replace by real directories.
    pub unfolds: BTreeSet<RelPath>,
}

impl Folding {
    /// True when a fold already deploys this source.
    pub fn covered(&self, source: &Source) -> bool {
        source.scope == Scope::Home && below_any(&source.path, &self.folds)
    }

    /// The unfolding directory this source lies below.
    pub fn unfolding(&self, source: &Source) -> Option<&RelPath> {
        dir_above(source, &self.unfolds)
    }
}

/// The directory of `dirs` that a home source's destination lies below.
pub fn dir_above<'a>(source: &Source, dirs: &'a BTreeSet<RelPath>) -> Option<&'a RelPath> {
    if !is_home(source) {
        return None;
    }
    ancestors(&source.dest).into_iter().find_map(|a| dirs.get(&a))
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

/// A direct child of home, or of `~/.local` (`share`, `state`, `bin`, ...):
/// on a new machine these are often missing, and a fold would let
/// programs write keys, tokens and history into the repository.
pub fn never_folds(dir: &RelPath) -> bool {
    let mut parts = dir.components();
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (_, None, _) | (Some(".local"), Some(_), None)
    )
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
) -> Result<BTreeSet<RelPath>> {
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
        .filter(|d| !never_folds(d))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    candidates.sort_by_key(|d| (d.components().count(), d.clone()));
    let mut folds = BTreeSet::new();
    for dir in candidates {
        if below_any(&dir, &folds) {
            continue;
        }
        // Keeping rustow's own link changes nothing; a new fold must not
        // expose sources that this run does not select.
        let record = recorded.get(&dir).copied();
        let allowed = if record.is_some() { &link_sources } else { &chosen };
        if qualifies(ctx, &dir, record.is_some(), allowed, collided)? {
            folds.insert(dir);
        }
    }
    Ok(folds)
}

/// `dir` can be one symlink: no home destination collides at or below it,
/// its destination allows a fold, and it holds only `allowed` sources.
fn qualifies(
    ctx: &Context,
    dir: &RelPath,
    recorded: bool,
    allowed: &BTreeSet<&RelPath>,
    collided: &Collided,
) -> Result<bool> {
    let collides = collided
        .keys()
        .any(|(scope, dest)| *scope == Scope::Home && dest.starts_with(dir));
    Ok(!collides && dest_allows_fold(ctx, dir, recorded)? && holds_only(ctx, dir, allowed)?)
}

/// Recorded folded links that a selected source now lies below and that are no longer folded.
fn select_unfolds(
    selected: &[&Source],
    recorded: &BTreeMap<RelPath, &Record>,
    folds: &BTreeSet<RelPath>,
) -> BTreeSet<RelPath> {
    let above_selected: BTreeSet<RelPath> = selected
        .iter()
        .filter(|s| is_home(s))
        .flat_map(|s| ancestors(&s.dest))
        .collect();
    recorded
        .keys()
        .filter(|dir| above_selected.contains(*dir) && !folds.contains(*dir))
        .cloned()
        .collect()
}

/// True when an ancestor directory of `path` is one of `dirs`.
fn below_any(path: &RelPath, dirs: &BTreeSet<RelPath>) -> bool {
    ancestors(path).iter().any(|a| dirs.contains(a))
}

/// Repository files below `dir` that are not one of `sources` (the
/// discovered source paths): they would vanish from the destination if
/// the folded link were unfolded.
pub fn strays(ctx: &Context, sources: &HashSet<&RelPath>, dir: &RelPath) -> Result<Vec<RelPath>> {
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

/// The directories above `path`, the nearest first.
pub fn ancestors(path: &RelPath) -> Vec<RelPath> {
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

/// The destination of `dir` is missing (with usable parents) or is a
/// folded link that rustow keeps: a recorded fold that `analyze` kept
/// (`recorded`: its link is still in place), or a symlink without a record
/// that reaches this repository directory. The plan adopts such a link, or
/// rewrites it in rustow's text.
fn dest_allows_fold(ctx: &Context, dir: &RelPath, recorded: bool) -> Result<bool> {
    match fs::symlink_metadata(ctx.home_target.join(dir.as_path())) {
        Err(_) => Ok(!matches!(
            fsops::parent_state(&ctx.home_target, dir, geteuid().as_raw())?,
            ParentState::Blocked { .. }
        )),
        Ok(m) if m.file_type().is_symlink() => Ok(recorded || reaches_dir(ctx, dir)?),
        Ok(_) => Ok(false),
    }
}

/// The symlink at the destination of `dir` names the repository directory
/// `dir`, in any text. A link elsewhere is the user's.
fn reaches_dir(ctx: &Context, dir: &RelPath) -> Result<bool> {
    let ParentState::Ready { dir: parent, .. } = fsops::parent_state(&ctx.home_target, dir, geteuid().as_raw())? else {
        return Ok(false);
    };
    let link = parent.join(dir.file_name());
    let target = ctx.root.join(dir.as_path());
    Ok(fs::read_link(&link).is_ok_and(|text| same_link_target(&link, &text, &target)))
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
/// following links. A path that cannot be read counts as a file that is
/// not a source: no fold covers it, and no unfold hides it.
fn entries_below(ctx: &Context, dir: &RelPath) -> Result<Vec<(RelPath, bool)>> {
    let top = ctx.root.join(dir.as_path());
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&top)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
    {
        let (path, is_dir) = match &entry {
            Ok(e) => (e.path(), e.file_type().is_dir()),
            Err(e) => (e.path().unwrap_or(&top), false),
        };
        // A name that is not UTF-8 is never a source.
        out.push((crate::source::repo_relative(&ctx.root, path)?.0, is_dir));
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
