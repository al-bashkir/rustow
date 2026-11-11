//! Directory folding (spec §7.1). With `fold_directories = true`, a home
//! directory whose repository copy holds only ordinary link sources, and
//! that does not exist at the destination yet, is deployed as one symlink.
//! A folded link that no longer qualifies is unfolded into a real
//! directory, but only when every file it exposes is a source, so that no
//! content written through the link disappears from view.

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

pub fn analyze(ctx: &Context, discovery: &Discovery, selected: &[&Source], state: &State) -> Result<Folding> {
    let home = |s: &&Source| s.scope == Scope::Home;
    let link_sources: BTreeSet<&RelPath> = discovery
        .sources
        .iter()
        .filter(|s| home(s) && s.mode == Mode::Link)
        .map(|s| &s.path)
        .collect();
    let recorded = recorded_folds(ctx, state);
    let mut folding = Folding::default();
    if ctx.config.defaults.fold_directories {
        let mut candidates: Vec<RelPath> = selected
            .iter()
            .filter(|s| home(s) && s.mode == Mode::Link)
            .flat_map(|s| ancestors(&s.path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        candidates.sort_by_key(|d| (d.components().count(), d.clone()));
        for dir in candidates {
            if folding.folds.iter().any(|f| dir.starts_with(f)) {
                continue;
            }
            if dest_allows_fold(ctx, &dir, recorded.get(&dir).copied())? && holds_only(ctx, &dir, &link_sources)? {
                folding.folds.push(dir);
            }
        }
    }
    for dir in recorded.keys() {
        let touched = selected.iter().any(|s| home(s) && s.dest.strip_prefix(dir).is_some());
        if touched && !folding.folds.contains(dir) {
            folding.unfolds.push(dir.clone());
        }
    }
    Ok(folding)
}

/// Repository files below `dir` that are not discovered sources: they
/// would vanish from the destination if the folded link were unfolded.
pub fn strays(ctx: &Context, discovery: &Discovery, dir: &RelPath) -> Result<Vec<RelPath>> {
    let sources: BTreeSet<&RelPath> = discovery.sources.iter().map(|s| &s.path).collect();
    Ok(files_below(ctx, dir)?
        .into_iter()
        .filter(|f| !sources.contains(f))
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

/// The destination of `dir` is missing (with usable parents) or is still
/// rustow's own folded link.
fn dest_allows_fold(ctx: &Context, dir: &RelPath, record: Option<&Record>) -> Result<bool> {
    let dest = ctx.home_target.join(dir.as_path());
    match fs::symlink_metadata(&dest) {
        Err(_) => Ok(!matches!(
            fsops::parent_state(&ctx.home_target, dir)?,
            ParentState::Blocked { .. }
        )),
        Ok(m) if m.file_type().is_symlink() => {
            let text = fs::read_link(&dest).ok();
            Ok(record.is_some_and(|r| r.link_target.is_some() && r.link_target == text))
        }
        Ok(_) => Ok(false),
    }
}

/// Every non-directory entry below `dir` is one of `allowed`.
fn holds_only(ctx: &Context, dir: &RelPath, allowed: &BTreeSet<&RelPath>) -> Result<bool> {
    Ok(files_below(ctx, dir)?.iter().all(|f| allowed.contains(f)))
}

/// Repository-relative paths of all non-directory entries below `dir`
/// (symlinks and special files included), without following links.
fn files_below(ctx: &Context, dir: &RelPath) -> Result<Vec<RelPath>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(ctx.root.join(dir.as_path()))
        .min_depth(1)
        .follow_links(false)
    {
        let entry = entry.map_err(|e| Error::Failed(format!("scan {dir}: {e}")))?;
        if entry.file_type().is_dir() {
            continue;
        }
        let rel = entry.path().strip_prefix(&ctx.root).unwrap_or(entry.path());
        let rel = rel
            .to_str()
            .ok_or_else(|| Error::Config(format!("non-UTF-8 path below {dir}")))?;
        out.push(RelPath::parse(rel)?);
    }
    Ok(out)
}
