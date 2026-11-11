//! Deployment planning. `decide` is the pure rule table that says what
//! may happen at one path; the planner feeds it real filesystem state.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::config::{Metadata, Owner};
use crate::error::{Error, Result};
use crate::fsops::{self, FileMeta, Observed, ParentState};
use crate::hash::Hash;
use crate::paths::{RelPath, Scope, link_text};
use crate::repo::Context;
use crate::source::{Discovery, Mode, Selector, Source, check_collisions};
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
    /// A system symlink would point into a cache that other users control.
    UntrustedCache,
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
            Conflict::UntrustedCache => "cache entry is not safe to link from a system path",
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
        (Desired::Symlink(want), Observed::Symlink(actual)) => match recorded {
            _ if actual == want && recorded == Recorded::Link(want.as_path()) => Decision::UpToDate,
            _ if actual == want => Decision::Adopt,
            Recorded::Link(ours) if ours == actual => Decision::Update,
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        (Desired::Symlink(_), Observed::File { hash, .. }) => match recorded {
            Recorded::Copy(ours) if ours == hash => Decision::Conflict(Conflict::FileToSymlink),
            Recorded::Copy(_) => Decision::Conflict(Conflict::Modified),
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        (Desired::File { .. }, Observed::Symlink(actual)) => match recorded {
            Recorded::Link(ours) if ours == actual => Decision::Update,
            _ => Decision::Conflict(Conflict::Unmanaged),
        },
        (Desired::File { hash: want, meta }, Observed::File { hash, mode, uid, gid }) => {
            if hash == want {
                match (meta.matches(*mode, *uid, *gid), recorded) {
                    (false, _) => Decision::FixMetadata,
                    (true, Recorded::Copy(ours)) if ours == want => Decision::UpToDate,
                    (true, _) => Decision::Adopt,
                }
            } else {
                match recorded {
                    Recorded::Copy(ours) if ours == hash => Decision::Update,
                    Recorded::Copy(_) => Decision::Conflict(Conflict::Modified),
                    _ => Decision::Conflict(Conflict::Unmanaged),
                }
            }
        }
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

/// Plans `selected` sources. Collisions are checked across all of
/// `discovery`, so a selected source that collides with an unselected
/// one is still blocked.
pub fn build(ctx: &Context, discovery: &Discovery, selected: &[&Source], state: &State) -> Result<Plan> {
    let mut collided: BTreeMap<(Scope, &RelPath), String> = BTreeMap::new();
    for c in check_collisions(&discovery.sources) {
        let detail = c.to_string();
        for s in discovery.sources.iter().filter(|s| c.sources.contains(&s.path)) {
            collided.insert((s.scope, &s.dest), detail.clone());
        }
    }
    let folding = fold::analyze(ctx, discovery, selected, state)?;
    let mut items = Vec::with_capacity(selected.len());
    for dir in &folding.folds {
        items.push(plan_fold(ctx, dir, state)?);
    }
    for dir in &folding.unfolds {
        items.push(plan_unfold(ctx, discovery, dir, state)?);
    }
    for source in selected.iter().filter(|s| !folding.covered(s)) {
        let dest = ctx.dest_path(source);
        let step = match collided.get(&(source.scope, &source.dest)) {
            Some(detail) => Step::Blocked {
                conflict: Conflict::Collision,
                detail: detail.clone(),
            },
            None => plan_one(ctx, source, &dest, state.entries.get(&dest), folding.unfolding(source))?,
        };
        items.push(PlanItem {
            source: (*source).clone(),
            dest,
            step,
        });
    }
    items.sort_by(|a, b| (a.source.scope, &a.source.path).cmp(&(b.source.scope, &b.source.path)));
    Ok(Plan { items })
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

fn plan_one(
    ctx: &Context,
    source: &Source,
    dest: &Path,
    record: Option<&Record>,
    unfolding: Option<&RelPath>,
) -> Result<Step> {
    let (path, missing_dirs) = match locate(ctx, source, unfolding)? {
        ParentState::Ready(parent) => (parent.join(source.dest.file_name()), None),
        ParentState::Blocked { path, reason } => {
            let detail = format!("{}: {reason}", path.display());
            return Ok(Step::Blocked {
                conflict: Conflict::ParentBlocked,
                detail,
            });
        }
        ParentState::Missing { .. } if !ctx.config.defaults.create_directories => {
            return Ok(Step::Blocked {
                conflict: Conflict::NoParent,
                detail: dest.display().to_string(),
            });
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
        return Ok(Step::Blocked {
            conflict: Conflict::ParentBlocked,
            detail,
        });
    }
    let want = match want(ctx, source, &path) {
        Ok(w) => w,
        Err(msg) => return Ok(Step::Failed(msg)),
    };
    if let (Scope::Root, false, Some((cache_path, _))) = (source.scope, source.mode.is_copy(), &want.cache) {
        let dir = missing_dirs
            .as_ref()
            .map_or_else(|| path.parent().unwrap_or(&path), |(existing, _)| existing);
        if let Err(why) = fsops::trust_cache(cache_path, fsops::owner_of(dir)?) {
            let detail = format!("{why}; copy it instead with [[template_rules]] metadata or `rustow file add`");
            return Ok(Step::Blocked {
                conflict: Conflict::UntrustedCache,
                detail,
            });
        }
    }
    let observed = match missing_dirs {
        Some(_) => Observed::Missing,
        None => match fsops::inspect(&path) {
            Ok(o) => o,
            Err(e) => return Ok(Step::Failed(with_privilege_hint(e))),
        },
    };
    let decision = decide(&want.desired, &observed, recorded(record));
    let detail = broken_link_detail(&path, &observed, decision);
    let cache = match want.cache {
        Some((cache_path, bytes)) => Some(plan_cache(cache_path, bytes, record, !source.mode.is_copy())?),
        None => None,
    };
    let file_mode = match (&observed, &want.desired) {
        (Observed::File { mode, .. }, Desired::File { meta, .. }) if meta.mode.is_none() => *mode,
        _ => want.file_mode,
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
        cache,
        record: want.record,
        detail,
    })))
}

/// Decision for a cache entry. A `guarded` entry backs a symlink, so a
/// change rustow did not make is a user edit (conflict); an entry that
/// only feeds a copy may simply be refreshed.
fn plan_cache(path: PathBuf, bytes: Zeroizing<Vec<u8>>, record: Option<&Record>, guarded: bool) -> Result<CacheWrite> {
    let observed = fsops::inspect(&path)?;
    let desired = Desired::File {
        hash: Hash::of(&bytes),
        meta: FileMeta::default(),
    };
    let previous = match (&observed, guarded) {
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
    let record = Record {
        source: source.path.clone(),
        scope: source.scope,
        mode: source.mode.label().to_owned(),
        backend: source.mode.backend(),
        source_hash: Hash::of(&bytes),
        applied_hash: None,
        render_id: None,
        link_target: None,
        cache_path: None,
        meta: None,
        folded_dir: false,
    };
    match &source.mode {
        Mode::Link => {
            let text = link_text(path, &source_abs);
            let record = Record {
                link_target: Some(text.clone()),
                ..record
            };
            Ok(Want {
                desired: Desired::Symlink(text),
                content: None,
                file_mode: 0o644,
                cache: None,
                record,
            })
        }
        Mode::FileCopy => {
            let mode = fs::symlink_metadata(&source_abs).map_or(0o644, |m| m.permissions().mode() & 0o777);
            let hash = Hash::of(&bytes);
            let record = Record {
                applied_hash: Some(hash.clone()),
                ..record
            };
            let desired = Desired::File {
                hash,
                meta: FileMeta::default(),
            };
            Ok(Want {
                desired,
                content: Some(Zeroizing::new(bytes)),
                file_mode: mode,
                cache: None,
                record,
            })
        }
        Mode::DecryptLink(backend) | Mode::DecryptCopy(backend) => {
            let plaintext = crypto::decrypt(*backend, &source.path, &bytes, ctx)?;
            transformed(ctx, source, path, plaintext, record)
        }
        Mode::TemplateLink(backend) | Mode::TemplateCopy(backend, _) => {
            // Encrypted templates are decrypted (and authenticated) before rendering.
            let plaintext = match backend {
                Some(b) => crypto::decrypt(*b, &source.path, &bytes, ctx)?,
                None => Zeroizing::new(bytes),
            };
            let text =
                std::str::from_utf8(&plaintext).map_err(|_| format!("template {} is not valid UTF-8", source.path))?;
            let context = template::context(&ctx.config.vars, &ctx.home_target);
            let rendered = template::render(source.path.as_str(), text, &context)?;
            let record = Record {
                render_id: Some(template::render_id(&plaintext, &context)),
                ..record
            };
            transformed(ctx, source, path, Zeroizing::new(rendered.into_bytes()), record)
        }
    }
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
    let mut out = Vec::new();
    for (dest, record) in &state.entries {
        let wanted = prefixes.is_empty() || prefixes.iter().any(|p| record.source.starts_with(p));
        let Ok(rel) = dest.strip_prefix(ctx.target_root(record.scope)) else {
            continue;
        };
        if !wanted || !selector.includes_label(&record.mode) {
            continue;
        }
        let rel = RelPath::parse(&rel.to_string_lossy())?;
        let (path, observed) = match fsops::parent_state(ctx.target_root(record.scope), &rel)? {
            ParentState::Ready(parent) => {
                let path = parent.join(rel.file_name());
                let observed = fsops::inspect(&path)?;
                (path, observed)
            }
            _ => (dest.clone(), Observed::Missing),
        };
        let verdict = match (&observed, recorded(Some(record))) {
            (Observed::Missing, _) => Verdict::Forget,
            (Observed::Symlink(t), Recorded::Link(ours)) if t == ours => Verdict::Remove(observed),
            (Observed::File { hash, .. }, Recorded::Copy(ours)) if hash == ours => Verdict::Remove(observed),
            _ => Verdict::Keep("destination changed since rustow deployed it; left in place".into()),
        };
        out.push(Removal {
            source: record.source.clone(),
            mode: record.mode.clone(),
            dest: dest.clone(),
            path,
            verdict,
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
    fn conflict_resolvability() {
        assert!(super::Conflict::Unmanaged.resolvable());
        assert!(super::Conflict::Modified.resolvable());
        assert!(super::Conflict::FileToSymlink.resolvable());
        assert!(!super::Conflict::Directory.resolvable());
        assert!(!super::Conflict::Special.resolvable());
    }
}
