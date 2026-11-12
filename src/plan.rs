//! Deployment planning. `decide` is the pure rule table that says what
//! may happen at one path; the planner feeds it real filesystem state.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::config::{Backend, Metadata, Owner};
use crate::error::{Error, Result};
use crate::fsops::{self, FileMeta, Observed, ParentState};
use crate::hash::Hash;
use crate::paths::{RelPath, Scope, link_text};
use crate::repo::Context;
use crate::source::{Discovery, Mode, Selector, Source, check_collisions, collision_key, shown_label};
use crate::state::{Record, State};
use crate::{cache, crypto, fold, template};

/// What a path should be after deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desired {
    /// A symlink with this exact text.
    Symlink(PathBuf),
    /// A regular file with this content and explicit metadata.
    File { hash: Hash, meta: FileMeta },
}

/// What rustow last applied at the path, from state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded<'a> {
    None,
    /// rustow created a symlink with this text.
    Link(&'a Path),
    /// rustow wrote a file with this content.
    Copy(&'a Hash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Something rustow did not create is in the way.
    Unmanaged,
    /// A file rustow wrote was changed by someone else.
    Modified,
    /// An unmodified managed copy would become a symlink (needs authorization).
    FileToSymlink,
    /// A directory is in the way; never replaced.
    Directory,
    /// A FIFO, socket or device is in the way; never replaced.
    Special,
    /// Several sources map to this destination.
    Collision,
    /// An ancestor directory is a file or an unsafe symlink.
    ParentBlocked,
    /// Parent directories are missing and `create_directories = false`.
    NoParent,
    /// A symlink would point at a file that someone other than root or the
    /// destination directory's owner can change.
    Untrusted,
    /// A folded directory holds files that are not sources.
    FoldedStrays,
}

impl Conflict {
    pub fn describe(self) -> &'static str {
        match self {
            Conflict::Unmanaged => "existing destination is not managed by rustow",
            Conflict::Modified => "destination was modified since rustow last applied it",
            Conflict::FileToSymlink => "managed copy would be replaced by a symlink",
            Conflict::Directory => "a directory is in the way",
            Conflict::Special => "a special file is in the way",
            Conflict::Collision => "several sources map to this destination",
            Conflict::ParentBlocked => "a parent directory is blocked",
            Conflict::NoParent => "parent directory is missing and create_directories = false",
            Conflict::Untrusted => "untrusted link target",
            Conflict::FoldedStrays => "cannot unfold the folded directory",
        }
    }

    /// Conflicts that `--backup` / `--force` may resolve.
    pub fn resolvable(self) -> bool {
        matches!(self, Conflict::Unmanaged | Conflict::Modified | Conflict::FileToSymlink)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Already correct and recorded.
    UpToDate,
    /// Already correct but not recorded: record it, change nothing.
    Adopt,
    /// Nothing there yet.
    Create,
    /// rustow's own unmodified object: replace it.
    Update,
    /// Content is right; owner or mode must change.
    FixMetadata,
    Conflict(Conflict),
}

/// The rule table (see docs/architecture.md). Pure: no I/O.
pub fn decide(desired: &Desired, observed: &Observed, recorded: Recorded) -> Decision {
    match (desired, observed) {
        (_, Observed::Missing) => Decision::Create,
        (_, Observed::Dir) => Decision::Conflict(Conflict::Directory),
        (_, Observed::Other) => Decision::Conflict(Conflict::Special),
        (Desired::Symlink(want), _) => decide_symlink(want, observed, recorded),
        (Desired::File { hash, meta }, _) => decide_file(hash, meta, observed, recorded),
    }
}

/// Rules for a desired symlink over a symlink or a regular file.
fn decide_symlink(want: &Path, observed: &Observed, recorded: Recorded) -> Decision {
    match observed {
        Observed::Symlink(actual) => symlink_over_link(want, actual, recorded),
        Observed::File { hash, .. } => match recorded {
            Recorded::Copy(ours) if ours == hash => Decision::Conflict(Conflict::FileToSymlink),
            Recorded::Copy(_) => Decision::Conflict(Conflict::Modified),
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        // `decide` handles the other kinds before it gets here.
        _ => Decision::Conflict(Conflict::Special),
    }
}

fn symlink_over_link(want: &Path, actual: &Path, recorded: Recorded) -> Decision {
    match recorded {
        _ if actual == want && recorded == Recorded::Link(want) => Decision::UpToDate,
        _ if actual == want => Decision::Adopt,
        Recorded::Link(ours) if ours == actual => Decision::Update,
        _ => Decision::Conflict(Conflict::Unmanaged),
    }
}

/// Rules for a desired regular file over a symlink or a regular file.
fn decide_file(want: &Hash, meta: &FileMeta, observed: &Observed, recorded: Recorded) -> Decision {
    match observed {
        Observed::Symlink(actual) => match recorded {
            Recorded::Link(ours) if ours == actual => Decision::Update,
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        Observed::File { hash, mode, uid, gid } if hash == want => {
            file_same_content(want, meta.matches(*mode, *uid, *gid), recorded)
        }
        Observed::File { hash, .. } => file_other_content(hash, recorded),
        // `decide` handles the other kinds before it gets here.
        _ => Decision::Conflict(Conflict::Special),
    }
}

/// The file already has the wanted content; `meta_ok` says whether owner and mode match.
fn file_same_content(want: &Hash, meta_ok: bool, recorded: Recorded) -> Decision {
    match (meta_ok, recorded) {
        (false, _) => Decision::FixMetadata,
        (true, Recorded::Copy(ours)) if ours == want => Decision::UpToDate,
        (true, _) => Decision::Adopt,
    }
}

fn file_other_content(hash: &Hash, recorded: Recorded) -> Decision {
    match recorded {
        Recorded::Copy(ours) if ours == hash => Decision::Update,
        Recorded::Copy(_) => Decision::Conflict(Conflict::Modified),
        _ => Decision::Conflict(Conflict::Unmanaged),
    }
}

/// What a planner reads from the record of one destination.
pub fn recorded(record: Option<&Record>) -> Recorded<'_> {
    match record {
        Some(Record {
            link_target: Some(t), ..
        }) => Recorded::Link(t),
        Some(Record {
            applied_hash: Some(h), ..
        }) => Recorded::Copy(h),
        _ => Recorded::None,
    }
}

/// A planned cache write behind a cache-backed destination.
#[derive(Debug)]
pub struct CacheWrite {
    pub path: PathBuf,
    pub bytes: Zeroizing<Vec<u8>>,
    pub observed: Observed,
    pub decision: Decision,
}

/// Everything the executor needs to deploy one source.
#[derive(Debug)]
pub struct Deploy {
    /// Real destination path (ancestor symlinks resolved).
    pub path: PathBuf,
    /// Deepest existing ancestor and the directories to create below it.
    pub missing_dirs: Option<(PathBuf, Vec<String>)>,
    pub dir_mode: u32,
    pub desired: Desired,
    pub observed: Observed,
    pub decision: Decision,
    /// Content for a regular-file destination.
    pub content: Option<Zeroizing<Vec<u8>>>,
    /// Mode for a regular-file destination when `desired` sets none.
    pub file_mode: u32,
    /// Owner and group for a regular-file destination (see `kept_owner`).
    pub file_owner: (Option<u32>, Option<u32>),
    pub cache: Option<CacheWrite>,
    pub record: Record,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum Step {
    Deploy(Box<Deploy>),
    /// Cannot be planned because of the destination's surroundings.
    Blocked {
        conflict: Conflict,
        detail: String,
    },
    /// Desired content could not be computed (render or decrypt failed).
    Failed(String),
    /// Replace rustow's folded directory symlink at `path` by a real directory.
    Unfold {
        path: PathBuf,
        observed: Observed,
    },
}

#[derive(Debug)]
pub struct PlanItem {
    pub source: Source,
    /// Destination as configured (target root + relative path); state key.
    pub dest: PathBuf,
    pub step: Step,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

/// What the desired destination looks like for one source.
struct Want {
    desired: Desired,
    content: Option<Zeroizing<Vec<u8>>>,
    file_mode: u32,
    cache: Option<(PathBuf, Zeroizing<Vec<u8>>)>,
    record: Record,
}

/// Whether each scope's target ignores case (see `case_insensitive`).
pub fn case_folding(ctx: &Context) -> impl Fn(Scope) -> bool + use<> {
    let home = case_insensitive(&ctx.home_target);
    let root = case_insensitive(&ctx.root_target);
    move |scope| match scope {
        Scope::Home => home,
        Scope::Root => root,
    }
}

/// True when the filesystem at `dir` ignores case: the same directory is
/// found again with the case of one path component swapped. A path with
/// no letter to swap counts as case-sensitive.
/// shortcut: "/" has no component, so a root target of "/" counts as
/// case-sensitive; probe a known entry if a case-insensitive "/" matters.
fn case_insensitive(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let id = |p: &Path| fs::metadata(p).ok().map(|m| (m.dev(), m.ino()));
    for path in dir.ancestors() {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            continue;
        };
        let upper = name.to_uppercase();
        let swapped = if upper != name { upper } else { name.to_lowercase() };
        if swapped != name {
            return id(path).is_some() && id(path) == id(&parent.join(swapped));
        }
    }
    false
}

/// Collision text for every source involved in a collision. `dest_path`
/// gives a source's absolute destination; `folded` tells whether a
/// scope's target ignores case.
fn collision_details(
    discovery: &Discovery,
    dest_path: impl Fn(&Source) -> PathBuf,
    folded: impl Fn(Scope) -> bool,
) -> BTreeMap<(Scope, &RelPath), String> {
    let mut collided = BTreeMap::new();
    for c in check_collisions(&discovery.sources, &folded) {
        let detail = c.to_string();
        for s in discovery.sources.iter().filter(|s| c.sources.contains(&s.path)) {
            collided.insert((s.scope, &s.dest), detail.clone());
        }
    }
    // The home and root targets can overlap (`.rustowroot/home/<user>/x`).
    let mut by_path: BTreeMap<PathBuf, Vec<&Source>> = BTreeMap::new();
    for s in &discovery.sources {
        let path = dest_path(s);
        let key = match folded(s.scope) {
            true => PathBuf::from(collision_key(&path.to_string_lossy(), true)),
            false => path,
        };
        by_path.entry(key).or_default().push(s);
    }
    for group in by_path.values().filter(|g| g.len() > 1) {
        let names: Vec<&str> = group.iter().map(|s| s.path.as_str()).collect();
        let detail = format!(
            "sources {} map to the same destination '{}'",
            names.join(", "),
            dest_path(group[0]).display()
        );
        for s in group {
            collided.entry((s.scope, &s.dest)).or_insert_with(|| detail.clone());
        }
    }
    collided
}

/// Plans `selected` sources. Collisions are checked across all of
/// `discovery`, so a selected source that collides with an unselected
/// one is still blocked.
pub fn build(ctx: &Context, discovery: &Discovery, selected: &[&Source], state: &State) -> Result<Plan> {
    let collided = collision_details(discovery, |s| ctx.dest_path(s), case_folding(ctx));
    let mut folding = fold::analyze(ctx, discovery, selected, state, &collided)?;
    let mut items = Vec::with_capacity(selected.len());
    for dir in &folding.folds {
        items.push(plan_fold(ctx, dir, state)?);
    }
    let blocked = plan_unfolds(ctx, discovery, &mut folding, state, &mut items)?;
    for source in selected.iter().filter(|s| !folding.covered(s)) {
        items.push(match below_blocked_unfold(ctx, source, &blocked) {
            Some(item) => item,
            None => plan_source(ctx, source, state, &collided, &folding)?,
        });
    }
    hold_partial_unfolds(discovery, &mut items);
    items.sort_by(|a, b| (a.source.scope, &a.source.path).cmp(&(b.source.scope, &b.source.path)));
    Ok(Plan { items })
}

/// One source that is not covered by a fold: blocked by a collision, or planned on its own.
fn plan_source(
    ctx: &Context,
    source: &Source,
    state: &State,
    collided: &BTreeMap<(Scope, &RelPath), String>,
    folding: &fold::Folding,
) -> Result<PlanItem> {
    let dest = ctx.dest_path(source);
    let step = match collided.get(&(source.scope, &source.dest)) {
        Some(detail) => blocked(Conflict::Collision, detail.clone()),
        None => plan_one(ctx, source, &dest, state.entries.get(&dest), folding.unfolding(source))?,
    };
    Ok(PlanItem {
        source: source.clone(),
        dest,
        step,
    })
}

/// A whole directory deployed as one symlink into the repository.
fn plan_fold(ctx: &Context, dir: &RelPath, state: &State) -> Result<PlanItem> {
    let source = Source {
        path: dir.clone(),
        scope: Scope::Home,
        dest: dir.clone(),
        mode: Mode::Link,
    };
    let dest = ctx.dest_path(&source);
    let mut step = plan_one(ctx, &source, &dest, state.entries.get(&dest), None)?;
    if let Step::Deploy(d) = &mut step {
        d.record.folded_dir = true;
        d.detail = Some("folded directory".into());
    }
    Ok(PlanItem { source, dest, step })
}

/// Replaces rustow's folded directory link by a real directory, unless
/// files written through the link would disappear from view.
/// Plans every unfold and returns the directories that cannot be
/// unfolded; only real unfolds stay in `folding.unfolds`.
fn plan_unfolds(
    ctx: &Context,
    discovery: &Discovery,
    folding: &mut fold::Folding,
    state: &State,
    items: &mut Vec<PlanItem>,
) -> Result<Vec<RelPath>> {
    let mut blocked = Vec::new();
    for dir in &folding.unfolds {
        let item = plan_unfold(ctx, discovery, dir, state)?;
        if !matches!(item.step, Step::Unfold { .. }) {
            blocked.push(dir.clone());
        }
        items.push(item);
    }
    folding.unfolds.retain(|d| !blocked.contains(d));
    Ok(blocked)
}

/// A source below a folded directory that cannot be unfolded is blocked
/// too: its destination path still runs through rustow's folded link.
fn below_blocked_unfold(ctx: &Context, source: &Source, blocked: &[RelPath]) -> Option<PlanItem> {
    let dir = blocked.iter().find(|d| is_below(source, d))?;
    Some(PlanItem {
        source: source.clone(),
        dest: ctx.dest_path(source),
        step: unfold_pending(dir),
    })
}

fn is_below(source: &Source, dir: &RelPath) -> bool {
    source.scope == Scope::Home && source.dest.strip_prefix(dir).is_some()
}

fn unfold_pending(dir: &RelPath) -> Step {
    blocked(
        Conflict::ParentBlocked,
        format!("the folded directory {dir} cannot be unfolded yet"),
    )
}

/// An unfold empties the folded directory, so it may run only when this
/// run deploys again every source below it. Otherwise the unfold becomes
/// a conflict and no deploy below it runs.
fn hold_partial_unfolds(discovery: &Discovery, items: &mut [PlanItem]) {
    let held: Vec<(RelPath, &RelPath)> = items
        .iter()
        .filter(|i| matches!(i.step, Step::Unfold { .. }))
        .filter_map(|i| Some((i.source.dest.clone(), not_redeployed(discovery, items, &i.source.dest)?)))
        .collect();
    for (dir, missing) in held {
        for item in items.iter_mut() {
            if item.source.path == dir {
                let detail = format!("{missing} would not be deployed again; run `rustow apply {dir}`");
                item.step = blocked(Conflict::FoldedStrays, detail);
            } else if is_below(&item.source, &dir) && matches!(item.step, Step::Deploy(_)) {
                item.step = unfold_pending(&dir);
            }
        }
    }
}

/// The first source below `dir` that `items` does not deploy without a conflict.
fn not_redeployed<'a>(discovery: &'a Discovery, items: &[PlanItem], dir: &RelPath) -> Option<&'a RelPath> {
    let clean: Vec<&RelPath> = items
        .iter()
        .filter(|i| deploys_cleanly(&i.step))
        .map(|i| &i.source.path)
        .collect();
    discovery
        .sources
        .iter()
        .filter(|s| is_below(s, dir))
        .map(|s| &s.path)
        .find(|p| !clean.contains(p))
}

fn deploys_cleanly(step: &Step) -> bool {
    let conflict = |d: &Decision| matches!(d, Decision::Conflict(_));
    matches!(step, Step::Deploy(d) if !conflict(&d.decision) && !d.cache.as_ref().is_some_and(|c| conflict(&c.decision)))
}

fn plan_unfold(ctx: &Context, discovery: &Discovery, dir: &RelPath, state: &State) -> Result<PlanItem> {
    let source = Source {
        path: dir.clone(),
        scope: Scope::Home,
        dest: dir.clone(),
        mode: Mode::Link,
    };
    let dest = ctx.dest_path(&source);
    let strays = fold::strays(ctx, discovery, dir)?;
    let link = state.entries.get(&dest).and_then(|r| r.link_target.clone());
    let step = match (fsops::parent_state(&ctx.home_target, dir)?, strays.is_empty()) {
        (_, false) => {
            let list: Vec<&str> = strays.iter().take(3).map(RelPath::as_str).collect();
            let detail = format!(
                "files that are not sources would disappear from view: {}",
                list.join(", ")
            );
            Step::Blocked {
                conflict: Conflict::FoldedStrays,
                detail,
            }
        }
        (ParentState::Ready(parent), true) => {
            let path = parent.join(dir.file_name());
            match fsops::inspect(&path)? {
                observed @ Observed::Symlink(_) if Some(&observed) == link.map(Observed::Symlink).as_ref() => {
                    Step::Unfold { path, observed }
                }
                _ => Step::Blocked {
                    conflict: Conflict::Unmanaged,
                    detail: "folded link changed".into(),
                },
            }
        }
        _ => Step::Blocked {
            conflict: Conflict::ParentBlocked,
            detail: dest.display().to_string(),
        },
    };
    Ok(PlanItem { source, dest, step })
}

/// Real destination path and the directories to create for it. Below an
/// unfolding directory, that directory counts as missing.
fn locate(ctx: &Context, source: &Source, unfolding: Option<&RelPath>) -> Result<ParentState> {
    let root = ctx.target_root(source.scope);
    let Some(dir) = unfolding else {
        return fsops::parent_state(root, &source.dest);
    };
    match fsops::parent_state(root, dir)? {
        ParentState::Ready(existing) => {
            let below = source.dest.parent().and_then(|p| p.strip_prefix(dir));
            let mut missing = vec![dir.file_name().to_owned()];
            missing.extend(below.iter().flat_map(|b| b.components().map(str::to_owned)));
            Ok(ParentState::Missing { existing, missing })
        }
        other => Ok(other),
    }
}

/// Directories to create: the deepest existing ancestor and the names below it.
type MissingDirs = Option<(PathBuf, Vec<String>)>;

/// Result of locating a destination: its path, or a step that ends planning early.
enum Located {
    At(PathBuf, MissingDirs),
    Early(Step),
}

fn blocked(conflict: Conflict, detail: String) -> Step {
    Step::Blocked { conflict, detail }
}

/// Finds the real destination path and the directories to create for it.
fn locate_dest(ctx: &Context, source: &Source, dest: &Path, unfolding: Option<&RelPath>) -> Result<Located> {
    let (path, missing_dirs) = match locate(ctx, source, unfolding)? {
        ParentState::Ready(parent) => (parent.join(source.dest.file_name()), None),
        ParentState::Blocked { path, reason } => {
            let detail = format!("{}: {reason}", path.display());
            return Ok(Located::Early(blocked(Conflict::ParentBlocked, detail)));
        }
        ParentState::Missing { .. } if !ctx.config.defaults.create_directories => {
            return Ok(Located::Early(blocked(Conflict::NoParent, dest.display().to_string())));
        }
        ParentState::Missing { existing, missing } => {
            let path = missing
                .iter()
                .fold(existing.clone(), |p, m| p.join(m))
                .join(source.dest.file_name());
            (path, Some((existing, missing)))
        }
    };
    if path.starts_with(&ctx.root) || path.starts_with(&ctx.cache_dir) {
        let detail = format!("{} resolves inside the repository or its cache", path.display());
        return Ok(Located::Early(blocked(Conflict::ParentBlocked, detail)));
    }
    Ok(Located::At(path, missing_dirs))
}

/// A symlink, in any scope, must point only at a file that nobody but
/// root or the destination directory's owner can change: the cache entry
/// of a cache-backed link, or the repository source of an ordinary link.
fn check_link_trust(
    ctx: &Context,
    source: &Source,
    path: &Path,
    missing_dirs: &MissingDirs,
    want: &Want,
) -> Result<Option<Step>> {
    if source.mode.is_copy() {
        return Ok(None);
    }
    let (target, what, fix) = match &want.cache {
        Some((cache_path, _)) => (
            cache_path.clone(),
            "cache entry",
            "[[template_rules]] metadata or `rustow file add`",
        ),
        None => (
            ctx.root.join(source.path.as_path()),
            "repository source",
            "`rustow file add`",
        ),
    };
    let dir = dest_dir(path, missing_dirs);
    Ok(fsops::trust_cache(&target, fsops::owner_of(dir)?).err().map(|why| {
        let detail = format!("{what}: {why}; copy it instead with {fix}");
        blocked(Conflict::Untrusted, detail)
    }))
}

/// The directory a destination is written into: its parent, or the
/// deepest existing ancestor while directories are still missing.
fn dest_dir<'a>(path: &'a Path, missing_dirs: &'a MissingDirs) -> &'a Path {
    missing_dirs
        .as_ref()
        .map_or_else(|| path.parent().unwrap_or(path), |(existing, _)| existing)
}

/// A decision that writes into a directory the effective user cannot
/// write fails already in the plan, so a dry run predicts the failure.
fn unwritable(path: &Path, missing_dirs: &MissingDirs, decision: Decision) -> Option<Step> {
    use nix::unistd::{AccessFlags, faccessat};
    let writes = match decision {
        Decision::Create | Decision::Update => true,
        Decision::Conflict(c) => c.resolvable(),
        _ => false,
    };
    if !writes {
        return None;
    }
    let dir = dest_dir(path, missing_dirs);
    let access = AccessFlags::W_OK | AccessFlags::X_OK;
    let errno = faccessat(nix::fcntl::AT_FDCWD, dir, access, nix::fcntl::AtFlags::AT_EACCESS).err()?;
    let err = Error::Io {
        context: format!("cannot write to directory {}", dir.display()),
        source: errno.into(),
    };
    Some(Step::Failed(with_privilege_hint(err)))
}

/// Reads the destination; a failed read becomes a `Failed` step.
fn observe_dest(path: &Path, missing_dirs: &MissingDirs) -> std::result::Result<Observed, Step> {
    if missing_dirs.is_some() {
        return Ok(Observed::Missing);
    }
    fsops::inspect(path).map_err(|e| Step::Failed(with_privilege_hint(e)))
}

/// Checks the cache trust, then reads the destination; `Err` carries the step that ends planning.
fn inspect_dest(
    ctx: &Context,
    source: &Source,
    path: &Path,
    missing_dirs: &MissingDirs,
    want: &Want,
) -> Result<std::result::Result<Observed, Step>> {
    if let Some(step) = check_link_trust(ctx, source, path, missing_dirs, want)? {
        return Ok(Err(step));
    }
    Ok(observe_dest(path, missing_dirs))
}

/// Mode for a regular-file destination: keep the existing mode unless one is set.
/// Owner and group for a rewritten regular file: explicit metadata wins;
/// otherwise an existing file keeps its owner and group (a root-run update
/// of /etc/app.conf must not turn root:app into root:root).
fn kept_owner(observed: &Observed, desired: &Desired) -> (Option<u32>, Option<u32>) {
    match (observed, desired) {
        (Observed::File { uid, gid, .. }, Desired::File { meta, .. }) => {
            (meta.uid.or(Some(*uid)), meta.gid.or(Some(*gid)))
        }
        (_, Desired::File { meta, .. }) => (meta.uid, meta.gid),
        _ => (None, None),
    }
}

fn file_mode_for(observed: &Observed, want: &Want) -> u32 {
    match (observed, &want.desired) {
        (Observed::File { mode, .. }, Desired::File { meta, .. }) if meta.mode.is_none() => *mode,
        _ => want.file_mode,
    }
}

fn plan_one(
    ctx: &Context,
    source: &Source,
    dest: &Path,
    record: Option<&Record>,
    unfolding: Option<&RelPath>,
) -> Result<Step> {
    let (path, missing_dirs) = match locate_dest(ctx, source, dest, unfolding)? {
        Located::At(path, missing_dirs) => (path, missing_dirs),
        Located::Early(step) => return Ok(step),
    };
    let want = match want(ctx, source, &path) {
        Ok(w) => w,
        Err(msg) => return Ok(Step::Failed(msg)),
    };
    let observed = match inspect_dest(ctx, source, &path, &missing_dirs, &want)? {
        Ok(o) => o,
        Err(step) => return Ok(step),
    };
    deploy_step(source, record, path, missing_dirs, want, observed)
}

/// Decides what to do at the destination and assembles the deploy step.
fn deploy_step(
    source: &Source,
    record: Option<&Record>,
    path: PathBuf,
    missing_dirs: MissingDirs,
    want: Want,
    observed: Observed,
) -> Result<Step> {
    let decision = decide(&want.desired, &observed, recorded(record));
    if let Some(step) = unwritable(&path, &missing_dirs, decision) {
        return Ok(step);
    }
    let detail = broken_link_detail(&path, &observed, decision);
    let file_mode = file_mode_for(&observed, &want);
    let file_owner = kept_owner(&observed, &want.desired);
    let cache = match want
        .cache
        .map(|(cache_path, bytes)| plan_cache(cache_path, bytes, record, !source.mode.is_copy()))
    {
        Some(Err(e)) => return Ok(Step::Failed(with_privilege_hint(e))),
        other => other.transpose()?,
    };
    Ok(Step::Deploy(Box::new(Deploy {
        path,
        missing_dirs,
        dir_mode: if source.mode.backend().is_some() { 0o700 } else { 0o777 },
        desired: want.desired,
        observed,
        decision,
        content: want.content,
        file_mode,
        file_owner,
        cache,
        record: want.record,
        detail,
    })))
}

/// Decision for a cache entry. An entry that backs a symlink (`links`) or
/// that backed the recorded symlink is guarded: a change rustow did not
/// make is a user edit (conflict). An entry that only feeds a copy may
/// simply be refreshed.
fn plan_cache(path: PathBuf, bytes: Zeroizing<Vec<u8>>, record: Option<&Record>, links: bool) -> Result<CacheWrite> {
    let observed = fsops::inspect(&path)?;
    let desired = Desired::File {
        hash: Hash::of(&bytes),
        meta: FileMeta::default(),
    };
    let linked = record.is_some_and(|r| r.link_target.is_some() && r.cache_path.as_ref() == Some(&path));
    let previous = match (&observed, links || linked) {
        (Observed::File { hash, .. }, false) => Recorded::Copy(hash),
        _ => record
            .and_then(|r| r.applied_hash.as_ref())
            .map_or(Recorded::None, Recorded::Copy),
    };
    let decision = decide(&desired, &observed, previous);
    Ok(CacheWrite {
        path,
        bytes,
        observed,
        decision,
    })
}

fn broken_link_detail(path: &Path, observed: &Observed, decision: Decision) -> Option<String> {
    let Observed::Symlink(text) = observed else {
        return None;
    };
    let dangling = path.parent().is_some_and(|p| fs::metadata(p.join(text)).is_err());
    (dangling && matches!(decision, Decision::Conflict(_))).then(|| format!("broken symlink to {}", text.display()))
}

/// Desired state of one source's destination at `path`.
fn want(ctx: &Context, source: &Source, path: &Path) -> std::result::Result<Want, String> {
    let source_abs = ctx.root.join(source.path.as_path());
    // A folded directory has no content of its own.
    let bytes = match source_abs.is_dir() {
        true => Vec::new(),
        false => fsops::read_file(&source_abs).map_err(|e| e.to_string())?,
    };
    let record = base_record(source, &bytes);
    match &source.mode {
        Mode::Link => Ok(want_link(path, &source_abs, record)),
        Mode::FileCopy => Ok(want_file_copy(&source_abs, bytes, record)),
        Mode::DecryptLink(backend) | Mode::DecryptCopy(backend) => {
            let plaintext = crypto::decrypt(*backend, &source.path, &bytes, ctx)?;
            transformed(ctx, source, path, plaintext, record)
        }
        Mode::TemplateLink(backend) | Mode::TemplateCopy(backend, _) => {
            want_template(ctx, source, path, *backend, bytes, record)
        }
    }
}

/// The record of a source before any mode-specific field is filled in.
fn base_record(source: &Source, bytes: &[u8]) -> Record {
    Record {
        source: source.path.clone(),
        scope: source.scope,
        mode: source.mode.label().to_owned(),
        backend: source.mode.backend(),
        source_hash: Hash::of(bytes),
        applied_hash: None,
        render_id: None,
        link_target: None,
        cache_path: None,
        meta: None,
        folded_dir: false,
    }
}

/// A symlink straight to the repository source.
fn want_link(path: &Path, source_abs: &Path, record: Record) -> Want {
    let text = link_text(path, source_abs);
    let record = Record {
        link_target: Some(text.clone()),
        ..record
    };
    Want {
        desired: Desired::Symlink(text),
        content: None,
        file_mode: 0o644,
        cache: None,
        record,
    }
}

/// A plain copy of the repository source.
fn want_file_copy(source_abs: &Path, bytes: Vec<u8>, record: Record) -> Want {
    let mode = fs::symlink_metadata(source_abs).map_or(0o644, |m| m.permissions().mode() & 0o777);
    let hash = Hash::of(&bytes);
    let record = Record {
        applied_hash: Some(hash.clone()),
        ..record
    };
    let desired = Desired::File {
        hash,
        meta: FileMeta::default(),
    };
    Want {
        desired,
        content: Some(Zeroizing::new(bytes)),
        file_mode: mode,
        cache: None,
        record,
    }
}

/// A template, decrypted first when encrypted, then rendered.
fn want_template(
    ctx: &Context,
    source: &Source,
    path: &Path,
    backend: Option<Backend>,
    bytes: Vec<u8>,
    record: Record,
) -> std::result::Result<Want, String> {
    // Encrypted templates are decrypted (and authenticated) before rendering.
    let plaintext = match backend {
        Some(b) => crypto::decrypt(b, &source.path, &bytes, ctx)?,
        None => Zeroizing::new(bytes),
    };
    let text = std::str::from_utf8(&plaintext).map_err(|_| format!("template {} is not valid UTF-8", source.path))?;
    let context = template::context(&ctx.config.vars, &ctx.home_target);
    let rendered = template::render(source.path.as_str(), text, &context)?;
    let record = Record {
        render_id: Some(template::render_id(&plaintext, &context)),
        ..record
    };
    transformed(ctx, source, path, Zeroizing::new(rendered.into_bytes()), record)
}

/// Desired state for content that goes through the cache: a symlink to
/// the cache entry, or a copy of it with explicit metadata.
fn transformed(
    ctx: &Context,
    source: &Source,
    path: &Path,
    content: Zeroizing<Vec<u8>>,
    record: Record,
) -> std::result::Result<Want, String> {
    let cache_path = cache::entry(ctx, source).ok_or("this mode has no cache entry")?;
    let hash = Hash::of(&content);
    let record = Record {
        applied_hash: Some(hash.clone()),
        cache_path: Some(cache_path.clone()),
        ..record
    };
    if source.mode.is_copy() {
        let meta = match &source.mode {
            Mode::TemplateCopy(_, m) => resolve_meta(m)?,
            _ => FileMeta::default(),
        };
        let file_mode = if source.mode.backend().is_some() { 0o600 } else { 0o644 };
        let record = Record {
            meta: Some(meta),
            ..record
        };
        let cache = Some((cache_path, content.clone()));
        Ok(Want {
            desired: Desired::File { hash, meta },
            content: Some(content),
            file_mode,
            cache,
            record,
        })
    } else {
        let text = link_text(path, &cache_path);
        let record = Record {
            link_target: Some(text.clone()),
            ..record
        };
        let cache = Some((cache_path, content));
        Ok(Want {
            desired: Desired::Symlink(text),
            content: None,
            file_mode: 0o600,
            cache,
            record,
        })
    }
}

/// Resolves owner and group names to ids on this system.
fn resolve_meta(meta: &Metadata) -> std::result::Result<FileMeta, String> {
    let uid = match &meta.owner {
        None => None,
        Some(Owner::Id(id)) => Some(*id),
        Some(Owner::Name(name)) => Some(
            nix::unistd::User::from_name(name)
                .ok()
                .flatten()
                .map(|u| u.uid.as_raw())
                .ok_or_else(|| format!("unknown user '{name}'"))?,
        ),
    };
    let gid = match &meta.group {
        None => None,
        Some(Owner::Id(id)) => Some(*id),
        Some(Owner::Name(name)) => Some(
            nix::unistd::Group::from_name(name)
                .ok()
                .flatten()
                .map(|g| g.gid.as_raw())
                .ok_or_else(|| format!("unknown group '{name}'"))?,
        ),
    };
    Ok(FileMeta {
        mode: meta.mode,
        uid,
        gid,
    })
}

/// Error text, with a hint when the cause is missing permission.
pub fn with_privilege_hint(err: Error) -> String {
    match &err {
        Error::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            format!("{err}; elevated privileges may be required (rustow never elevates itself)")
        }
        _ => err.to_string(),
    }
}

/// One managed destination to remove.
#[derive(Debug)]
pub struct Removal {
    pub source: RelPath,
    pub mode: String,
    /// State key.
    pub dest: PathBuf,
    /// Real path.
    pub path: PathBuf,
    pub verdict: Verdict,
}

#[derive(Debug)]
pub enum Verdict {
    /// Remove the object, which must still look like this.
    Remove(Observed),
    /// Already gone: only forget the record.
    Forget,
    /// Changed since rustow created it: left in place.
    Keep(String),
}

/// Plans removal of recorded destinations whose source matches `args`
/// (repository-relative prefixes; all when empty) and whose mode the
/// selector covers. Sources that no longer exist are included.
pub fn plan_removal(ctx: &Context, args: &[String], selector: Selector, state: &State) -> Result<Vec<Removal>> {
    let prefixes = parse_prefixes(ctx, args)?;
    let mut out = Vec::new();
    for (dest, record) in &state.entries {
        let wanted = prefixes.is_empty() || prefixes.iter().any(|p| record.source.starts_with(p));
        let Ok(rel) = dest.strip_prefix(ctx.target_root(record.scope)) else {
            continue;
        };
        if !wanted || !selector.includes_label(&record.mode) {
            continue;
        }
        let (path, observed) = observe_recorded(ctx, record, dest, rel)?;
        out.push(Removal {
            source: record.source.clone(),
            mode: shown_label(&record.mode, record.backend),
            dest: dest.clone(),
            path,
            verdict: removal_verdict(observed, record),
        });
    }
    if out.is_empty() && !prefixes.is_empty() {
        let list: Vec<&str> = prefixes.iter().map(RelPath::as_str).collect();
        return Err(Error::Usage(format!(
            "nothing deployed by rustow matches {}",
            list.join(", ")
        )));
    }
    Ok(out)
}

/// Parses removal arguments into source prefixes; `.` means everything.
fn parse_prefixes(ctx: &Context, args: &[String]) -> Result<Vec<RelPath>> {
    let mut prefixes = Vec::new();
    for arg in args.iter().filter(|a| !matches!(a.as_str(), "." | "./")) {
        let rel = RelPath::parse(arg)?;
        if ctx.ignore.is_reserved(&rel, ctx.root.join(rel.as_path()).is_dir()) {
            return Err(Error::Usage(format!(
                "'{rel}' is reserved by rustow and is never deployed"
            )));
        }
        prefixes.push(rel);
    }
    Ok(prefixes)
}

/// Pure: whether a recorded destination may be removed, given what is there now.
fn removal_verdict(observed: Observed, record: &Record) -> Verdict {
    match (&observed, recorded(Some(record))) {
        (Observed::Missing, _) => Verdict::Forget,
        (Observed::Symlink(t), Recorded::Link(ours)) if t == ours => Verdict::Remove(observed),
        (Observed::File { hash, .. }, Recorded::Copy(ours)) if hash == ours => Verdict::Remove(observed),
        // The user replaced rustow's folded link: nothing there is rustow's any more.
        _ if record.folded_dir => Verdict::Forget,
        _ => Verdict::Keep("destination changed since rustow deployed it; left in place".into()),
    }
}

/// Real path of a recorded destination and what is there now.
fn observe_recorded(ctx: &Context, record: &Record, dest: &Path, rel: &Path) -> Result<(PathBuf, Observed)> {
    let rel = RelPath::parse(&rel.to_string_lossy())?;
    match fsops::parent_state(ctx.target_root(record.scope), &rel)? {
        ParentState::Ready(parent) => {
            let path = parent.join(rel.file_name());
            let observed = fsops::inspect(&path)?;
            Ok((path, observed))
        }
        _ => Ok((dest.to_path_buf(), Observed::Missing)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    fn h(s: &str) -> Hash {
        Hash::of(s.as_bytes())
    }

    fn file(s: &str) -> Observed {
        Observed::File {
            hash: h(s),
            mode: 0o644,
            uid: 1000,
            gid: 1000,
        }
    }

    fn want_file(s: &str) -> Desired {
        Desired::File {
            hash: h(s),
            meta: FileMeta::default(),
        }
    }

    fn want_link(t: &str) -> Desired {
        Desired::Symlink(t.into())
    }

    fn link(t: &str) -> Observed {
        Observed::Symlink(t.into())
    }

    #[test]
    fn missing_is_create_for_both_kinds() {
        assert_eq!(decide(&want_link("a"), &Observed::Missing, Recorded::None), Create);
        assert_eq!(
            decide(&want_file("a"), &Observed::Missing, Recorded::Copy(&h("old"))),
            Create
        );
    }

    #[test]
    fn dir_and_special_always_conflict() {
        for d in [want_link("a"), want_file("a")] {
            assert_eq!(
                decide(&d, &Observed::Dir, Recorded::None),
                Conflict(super::Conflict::Directory)
            );
            assert_eq!(
                decide(&d, &Observed::Other, Recorded::Copy(&h("a"))),
                Conflict(super::Conflict::Special)
            );
        }
    }

    #[test]
    fn symlink_cases() {
        let t = PathBuf::from("dot/x");
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/x"), Recorded::Link(&t)),
            UpToDate
        );
        assert_eq!(decide(&want_link("dot/x"), &link("dot/x"), Recorded::None), Adopt);
        let old = PathBuf::from("dot/old");
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/old"), Recorded::Link(&old)),
            Update
        );
        assert_eq!(
            decide(&want_link("dot/x"), &link("elsewhere"), Recorded::Link(&old)),
            Conflict(super::Conflict::Unmanaged)
        );
        assert_eq!(
            decide(&want_link("dot/x"), &link("dot/old"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn file_to_symlink_is_conflict_even_if_unmodified() {
        assert_eq!(
            decide(&want_link("dot/x"), &file("a"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::FileToSymlink)
        );
        assert_eq!(
            decide(&want_link("dot/x"), &file("b"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::Modified)
        );
        assert_eq!(
            decide(&want_link("dot/x"), &file("a"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn our_symlink_to_file_is_update() {
        let t = PathBuf::from("dot/x");
        assert_eq!(decide(&want_file("a"), &link("dot/x"), Recorded::Link(&t)), Update);
        assert_eq!(
            decide(&want_file("a"), &link("other"), Recorded::Link(&t)),
            Conflict(super::Conflict::Unmanaged)
        );
        assert_eq!(
            decide(&want_file("a"), &link("dot/x"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn copy_cases() {
        assert_eq!(decide(&want_file("a"), &file("a"), Recorded::Copy(&h("a"))), UpToDate);
        // Unrecorded but identical: adopt.
        assert_eq!(decide(&want_file("a"), &file("a"), Recorded::None), Adopt);
        // Source changed, destination untouched: outdated, safe update.
        assert_eq!(
            decide(&want_file("new"), &file("old"), Recorded::Copy(&h("old"))),
            Update
        );
        // Destination changed (source same or changed): modified.
        assert_eq!(
            decide(&want_file("a"), &file("edited"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::Modified)
        );
        assert_eq!(
            decide(&want_file("new"), &file("edited"), Recorded::Copy(&h("a"))),
            Conflict(super::Conflict::Modified)
        );
        assert_eq!(
            decide(&want_file("a"), &file("b"), Recorded::None),
            Conflict(super::Conflict::Unmanaged)
        );
    }

    #[test]
    fn rewritten_copies_keep_owner_unless_set_explicitly() {
        let existing = Observed::File {
            hash: h("old"),
            mode: 0o640,
            uid: 0,
            gid: 1000,
        };
        let plain = want_file("new");
        assert_eq!(
            kept_owner(&existing, &plain),
            (Some(0), Some(1000)),
            "an update keeps root:group"
        );
        let explicit = Desired::File {
            hash: h("new"),
            meta: FileMeta {
                mode: None,
                uid: Some(5),
                gid: None,
            },
        };
        assert_eq!(kept_owner(&existing, &explicit), (Some(5), Some(1000)));
        assert_eq!(
            kept_owner(&Observed::Missing, &plain),
            (None, None),
            "new files belong to the runner"
        );
        assert_eq!(kept_owner(&existing, &want_link("t")), (None, None));
    }

    #[test]
    fn meta_only_change_is_fix_metadata() {
        let want = Desired::File {
            hash: h("a"),
            meta: FileMeta {
                mode: Some(0o600),
                uid: None,
                gid: None,
            },
        };
        assert_eq!(decide(&want, &file("a"), Recorded::Copy(&h("a"))), FixMetadata);
        assert_eq!(decide(&want, &file("a"), Recorded::None), FixMetadata);
        let ok = Observed::File {
            hash: h("a"),
            mode: 0o600,
            uid: 1,
            gid: 1,
        };
        assert_eq!(decide(&want, &ok, Recorded::Copy(&h("a"))), UpToDate);
    }

    #[test]
    fn case_probe_compares_the_swapped_name_with_the_real_one() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().canonicalize().unwrap().join("home");
        fs::create_dir(&dir).unwrap();
        assert!(!case_insensitive(&dir), "no HOME next to home");
        // A swapped-case name that reaches the same directory is what a case-insensitive filesystem shows.
        std::os::unix::fs::symlink(&dir, dir.with_file_name("HOME")).unwrap();
        assert!(case_insensitive(&dir));
        fs::create_dir(dir.join("42")).unwrap();
        assert!(
            case_insensitive(&dir.join("42")),
            "'42' has no letter, so 'home' decides"
        );
        assert!(!case_insensitive(Path::new("/")), "no letter to swap");
    }

    #[test]
    fn conflict_resolvability() {
        assert!(super::Conflict::Unmanaged.resolvable());
        assert!(super::Conflict::Modified.resolvable());
        assert!(super::Conflict::FileToSymlink.resolvable());
        assert!(!super::Conflict::Directory.resolvable());
        assert!(!super::Conflict::Special.resolvable());
    }

    /// The root target holds the home target here, so `.rustowroot/home/X`
    /// and the home source `x` meet only where the target ignores case.
    #[test]
    fn overlapping_targets_collide_by_case_only_where_case_is_ignored() {
        let source = |path: &str, scope, dest: &str| Source {
            path: RelPath::parse(path).unwrap(),
            scope,
            dest: RelPath::parse(dest).unwrap(),
            mode: Mode::Link,
        };
        let discovery = Discovery {
            sources: vec![
                source(".rustowroot/home/X", Scope::Root, "home/X"),
                source("x", Scope::Home, "x"),
            ],
            ..Default::default()
        };
        let dest_path = |s: &Source| match s.scope {
            Scope::Home => Path::new("/t/home").join(s.dest.as_path()),
            Scope::Root => Path::new("/t").join(s.dest.as_path()),
        };
        assert!(collision_details(&discovery, dest_path, |_| false).is_empty());
        let collided = collision_details(&discovery, dest_path, |_| true);
        let want = "sources .rustowroot/home/X, x map to the same destination '/t/home/X'";
        assert_eq!(collided.len(), 2, "{collided:?}");
        assert!(collided.values().all(|d| d == want), "{collided:?}");
    }
}
