//! Runs a plan. Each item is applied, verified and recorded on its own,
//! so a failure leaves earlier items deployed and recorded and later
//! items untouched. There is no global rollback.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::fsops::{self, Observed};
use crate::paths::RelPath;
use crate::plan::{Conflict, Decision, Deploy, Desired, Plan, PlanItem, Removal, Step, Verdict};
use crate::state::State;

/// What to do with a resolvable conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Policy {
    /// Change nothing at all when any item conflicts (default).
    Abort,
    /// Leave conflicting destinations as they are; deploy the rest.
    Skip,
    /// Move the conflicting destination into the backup directory, then deploy.
    Backup,
    /// Remove the conflicting file or symlink, then deploy.
    Overwrite,
}

/// Outcome of one item, as reported to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Unchanged,
    Adopt,
    Create,
    Update,
    Metadata,
    Replace,
    Remove,
    Forget,
    Conflict,
    Skip,
    Fail,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Unchanged => "ok",
            Action::Adopt => "adopt",
            Action::Create => "create",
            Action::Update => "update",
            Action::Metadata => "metadata",
            Action::Replace => "replace",
            Action::Remove => "remove",
            Action::Forget => "forget",
            Action::Conflict => "conflict",
            Action::Skip => "skip",
            Action::Fail => "error",
        }
    }

    /// True when the filesystem changes (or would change).
    pub fn changes(self) -> bool {
        matches!(
            self,
            Action::Create | Action::Update | Action::Metadata | Action::Replace | Action::Remove
        )
    }
}

#[derive(Debug, Clone)]
pub struct ItemReport {
    pub source: RelPath,
    pub dest: PathBuf,
    pub mode: String,
    pub action: Action,
    pub detail: Option<String>,
}

/// Exit code for a finished or predicted run (see docs/cli.md).
pub fn exit_code(items: &[ItemReport]) -> i32 {
    if items.iter().any(|i| i.action == Action::Fail) {
        4
    } else if items
        .iter()
        .any(|i| matches!(i.action, Action::Conflict | Action::Skip))
    {
        3
    } else {
        0
    }
}

/// True when the plan holds a conflict or failure that the policy does not resolve.
pub fn blocked(plan: &Plan, policy: Policy) -> bool {
    plan.items
        .iter()
        .any(|i| matches!(predict(i, policy).0, Action::Conflict | Action::Skip | Action::Fail))
}

/// The action `execute` would take for an item, and why.
pub fn predict(item: &PlanItem, policy: Policy) -> (Action, Option<String>) {
    match &item.step {
        Step::Failed(msg) => (Action::Fail, Some(msg.clone())),
        Step::Unfold { .. } => (
            Action::Update,
            Some("unfold the folded directory link into a real directory".into()),
        ),
        Step::Blocked { conflict, detail } => unresolved(policy, format!("{}: {detail}", conflict.describe())),
        Step::Deploy(d) => predict_deploy(d, policy),
    }
}

/// A conflict the policy cannot resolve: skipped under `Skip`, else a conflict.
fn unresolved(policy: Policy, detail: String) -> (Action, Option<String>) {
    let action = if policy == Policy::Skip {
        Action::Skip
    } else {
        Action::Conflict
    };
    (action, Some(detail))
}

fn conflict_of(decision: Decision) -> Option<Conflict> {
    match decision {
        Decision::Conflict(conflict) => Some(conflict),
        _ => None,
    }
}

/// The conflict of a deploy step (destination first, then cache) with its detail text.
fn deploy_conflict(d: &Deploy) -> Option<(Conflict, String)> {
    let dest = conflict_of(d.decision);
    let conflict = dest.or_else(|| d.cache.as_ref().and_then(|c| conflict_of(c.decision)))?;
    let what = if dest.is_some() { "" } else { "cache entry: " };
    let mut detail = format!("{what}{}", conflict.describe());
    if let Some(extra) = &d.detail {
        detail = format!("{detail} ({extra})");
    }
    Some((conflict, detail))
}

fn predict_deploy(d: &Deploy, policy: Policy) -> (Action, Option<String>) {
    if let Some((conflict, detail)) = deploy_conflict(d) {
        if conflict.resolvable() && matches!(policy, Policy::Backup | Policy::Overwrite) {
            return (Action::Replace, Some(detail));
        }
        return unresolved(policy, detail);
    }
    (deploy_action(d), d.detail.clone())
}

/// The action for a deploy step without conflicts.
fn deploy_action(d: &Deploy) -> Action {
    let cache_changes = d
        .cache
        .as_ref()
        .is_some_and(|c| c.decision != Decision::UpToDate && c.decision != Decision::Adopt);
    match d.decision {
        Decision::Create => Action::Create,
        Decision::Update => Action::Update,
        Decision::FixMetadata => Action::Metadata,
        _ if cache_changes => Action::Update,
        Decision::Adopt => Action::Adopt,
        _ => Action::Unchanged,
    }
}

/// Where backups of this run go, and how the executor stops early.
pub struct Run<'a> {
    pub policy: Policy,
    pub backup_dir: &'a Path,
    /// Cache root; checked (real 0700 directory owned by the user) before any cache write.
    pub cache_dir: &'a Path,
    pub stop: &'a AtomicBool,
}

/// Executes every item the policy allows and records successes in `state`.
pub fn execute(plan: Plan, run: &Run, state: &mut State) -> Vec<ItemReport> {
    let mut out = Vec::with_capacity(plan.items.len());
    for item in plan.items {
        let (mut action, mut detail) = predict(&item, run.policy);
        if run.stop.load(Ordering::SeqCst) {
            (action, detail) = (
                Action::Fail,
                Some("interrupted before this item; nothing changed".into()),
            );
        } else if let Step::Deploy(d) = &item.step
            && !matches!(action, Action::Conflict | Action::Skip | Action::Fail)
        {
            match apply(d, &item, run, state) {
                Ok(Some(extra)) => detail = Some(extra),
                Ok(None) => {}
                Err(e) => (action, detail) = (Action::Fail, Some(crate::plan::with_privilege_hint(e))),
            }
        } else if let Step::Unfold { path, observed } = &item.step
            && let Err(e) = unfold(path, observed, &item.dest, state)
        {
            (action, detail) = (Action::Fail, Some(crate::plan::with_privilege_hint(e)));
        }
        out.push(ItemReport {
            source: item.source.path.clone(),
            dest: item.dest.clone(),
            mode: item.source.mode.label().to_owned(),
            action,
            detail,
        });
    }
    out
}

fn apply(d: &Deploy, item: &PlanItem, run: &Run, state: &mut State) -> Result<Option<String>> {
    if let Some((existing, missing)) = &d.missing_dirs {
        state
            .created_dirs
            .extend(fsops::create_dirs(existing, missing, d.dir_mode)?);
    }
    let mut detail = apply_cache(d, item, run)?;
    if let Some(extra) = apply_dest(d, item, run)? {
        detail = Some(extra);
    }
    verify(d)?;
    state.entries.insert(item.dest.clone(), d.record.clone());
    Ok(detail)
}

/// Writes the cache entry when it changes; returns the backup note, if any.
fn apply_cache(d: &Deploy, item: &PlanItem, run: &Run) -> Result<Option<String>> {
    let Some(cache) = &d.cache else { return Ok(None) };
    if matches!(cache.decision, Decision::UpToDate | Decision::Adopt) {
        return Ok(None);
    }
    fsops::ensure_private_dir(run.cache_dir)?;
    recheck(&cache.path, &cache.observed)?;
    let mut detail = None;
    if let (Decision::Conflict(_), Policy::Backup) = (cache.decision, run.policy) {
        detail = Some(backup(&cache.path, &backup_path(run, item, true))?);
    }
    if let Some(parent) = cache.path.parent() {
        fsops::ensure_private_dir(parent)?;
    }
    fsops::write_atomic(&cache.path, &cache.bytes, 0o600, None, None)?;
    Ok(detail)
}

/// Performs the planned destination decision (the caller already checked
/// that the policy allows it); returns the backup note, if any.
fn apply_dest(d: &Deploy, item: &PlanItem, run: &Run) -> Result<Option<String>> {
    match d.decision {
        Decision::Create => create(d).map(|()| None),
        Decision::Update => {
            recheck(&d.path, &d.observed)?;
            place(d).map(|()| None)
        }
        Decision::Conflict(_) => {
            recheck(&d.path, &d.observed)?;
            let detail = clear_conflict(d, item, run)?;
            create(d)?;
            Ok(detail)
        }
        Decision::FixMetadata => fix_metadata(d).map(|()| None),
        Decision::UpToDate | Decision::Adopt => Ok(None),
    }
}

/// Creates the destination; fails if anything appeared there since planning.
fn create(d: &Deploy) -> Result<()> {
    match &d.desired {
        Desired::Symlink(text) => fsops::symlink_new(&d.path, text),
        Desired::File { meta, .. } => {
            let bytes = d.content.as_deref().map_or(&[][..], |b| b.as_slice());
            fsops::write_new(
                &d.path,
                bytes,
                meta.mode.unwrap_or(d.file_mode),
                d.file_owner.0,
                d.file_owner.1,
            )
        }
    }
}

fn fix_metadata(d: &Deploy) -> Result<()> {
    recheck(&d.path, &d.observed)?;
    if let Desired::File { meta, .. } = &d.desired {
        fsops::set_meta(&d.path, *meta)?;
    }
    Ok(())
}

/// Moves the conflicting destination to the backup dir, or removes it.
fn clear_conflict(d: &Deploy, item: &PlanItem, run: &Run) -> Result<Option<String>> {
    if run.policy == Policy::Backup {
        return backup(&d.path, &backup_path(run, item, false)).map(Some);
    }
    fs::remove_file(&d.path).map_err(|e| Error::Failed(format!("remove {}: {e}", d.path.display())))?;
    Ok(None)
}

fn backup(path: &Path, to: &Path) -> Result<String> {
    fsops::backup(path, to)?;
    Ok(format!("backup saved to {}", to.display()))
}

fn backup_path(run: &Run, item: &PlanItem, cache: bool) -> PathBuf {
    let scope = if cache { "cache" } else { item.source.scope.name() };
    run.backup_dir.join(scope).join(item.source.dest.as_path())
}

fn place(d: &Deploy) -> Result<()> {
    match &d.desired {
        Desired::Symlink(text) => fsops::symlink_atomic(&d.path, text),
        Desired::File { meta, .. } => {
            let bytes = d.content.as_deref().map_or(&[][..], |b| b.as_slice());
            fsops::write_atomic(
                &d.path,
                bytes,
                meta.mode.unwrap_or(d.file_mode),
                d.file_owner.0,
                d.file_owner.1,
            )
        }
    }
}

/// Fails when `path` changed since it was planned.
fn recheck(path: &Path, planned: &Observed) -> Result<()> {
    if fsops::inspect(path)? != *planned {
        return Err(Error::Failed(format!(
            "{} changed while rustow was running; left unchanged",
            path.display()
        )));
    }
    Ok(())
}

/// Confirms that the destination now is what was planned.
fn verify(d: &Deploy) -> Result<()> {
    let ok = match (&d.desired, fsops::inspect(&d.path)?) {
        (Desired::Symlink(want), Observed::Symlink(have)) => *want == have,
        (
            Desired::File { hash, meta },
            Observed::File {
                hash: have,
                mode,
                uid,
                gid,
            },
        ) => *hash == have && meta.matches(mode, uid, gid),
        _ => false,
    };
    if !ok {
        return Err(Error::Failed(format!(
            "{} does not match the planned result after writing",
            d.path.display()
        )));
    }
    Ok(())
}

/// Removes managed destinations and the empty directories rustow created for them.
pub fn remove(removals: Vec<Removal>, stop: &AtomicBool, state: &mut State) -> Vec<ItemReport> {
    let mut out = Vec::with_capacity(removals.len());
    for r in removals {
        let (action, detail) = if stop.load(Ordering::SeqCst) {
            (
                Action::Fail,
                Some("interrupted before this item; nothing changed".into()),
            )
        } else {
            match &r.verdict {
                Verdict::Keep(why) => (Action::Conflict, Some(why.clone())),
                Verdict::Forget => {
                    state.entries.remove(&r.dest);
                    (Action::Forget, Some("already gone".into()))
                }
                Verdict::Remove(observed) => match remove_one(&r.path, observed, state) {
                    Ok(()) => {
                        state.entries.remove(&r.dest);
                        (Action::Remove, None)
                    }
                    Err(e) => (Action::Fail, Some(e.to_string())),
                },
            }
        };
        out.push(ItemReport {
            source: r.source,
            dest: r.dest,
            mode: r.mode,
            action,
            detail,
        });
    }
    out
}

fn remove_one(path: &Path, observed: &Observed, state: &mut State) -> Result<()> {
    recheck(path, observed)?;
    fs::remove_file(path).map_err(|e| Error::Failed(format!("remove {}: {e}", path.display())))?;
    for dir in path.ancestors().skip(1) {
        if !state.created_dirs.contains(dir) || fs::remove_dir(dir).is_err() {
            break;
        }
        state.created_dirs.remove(dir);
    }
    Ok(())
}

/// Report lines for a removal plan without running it.
pub fn predict_removal(removals: &[Removal]) -> Vec<ItemReport> {
    removals
        .iter()
        .map(|r| {
            let (action, detail) = match &r.verdict {
                Verdict::Keep(why) => (Action::Conflict, Some(why.clone())),
                Verdict::Forget => (Action::Forget, Some("already gone".into())),
                Verdict::Remove(_) => (Action::Remove, None),
            };
            ItemReport {
                source: r.source.clone(),
                dest: r.dest.clone(),
                mode: r.mode.clone(),
                action,
                detail,
            }
        })
        .collect()
}
/// Replaces rustow's folded directory symlink by a real directory.
fn unfold(path: &Path, observed: &Observed, dest: &Path, state: &mut State) -> Result<()> {
    recheck(path, observed)?;
    fs::remove_file(path).map_err(|e| Error::Failed(format!("remove folded link {}: {e}", path.display())))?;
    fs::create_dir(path).map_err(|e| Error::Failed(format!("create directory {}: {e}", path.display())))?;
    state.created_dirs.insert(path.to_path_buf());
    state.entries.remove(dest);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::FileMeta;
    use crate::hash::Hash;
    use crate::paths::Scope;
    use crate::plan::Deploy;
    use crate::source::{Mode, Source};
    use crate::state::Record;

    fn record() -> Record {
        Record {
            source: RelPath::parse("a").unwrap(),
            scope: Scope::Home,
            mode: "link".into(),
            backend: None,
            source_hash: Hash::of(b""),
            applied_hash: None,
            render_id: None,
            link_target: None,
            cache_path: None,
            meta: None,
            folded_dir: false,
        }
    }

    /// A one-item plan that creates `path` (planned while it was missing).
    fn create_plan(path: &Path, desired: Desired, content: Option<&[u8]>) -> Plan {
        let source = Source {
            path: RelPath::parse("a").unwrap(),
            scope: Scope::Home,
            dest: RelPath::parse("a").unwrap(),
            mode: Mode::Link,
        };
        let deploy = Deploy {
            path: path.to_path_buf(),
            missing_dirs: None,
            dir_mode: 0o777,
            desired,
            observed: Observed::Missing,
            decision: Decision::Create,
            content: content.map(|c| zeroize::Zeroizing::new(c.to_vec())),
            file_mode: 0o644,
            file_owner: (None, None),
            cache: None,
            record: record(),
            detail: None,
        };
        Plan {
            items: vec![PlanItem {
                source,
                dest: path.to_path_buf(),
                step: Step::Deploy(Box::new(deploy)),
            }],
        }
    }

    fn run_plan(plan: Plan, dir: &Path) -> Vec<ItemReport> {
        let stop = AtomicBool::new(false);
        let run = Run {
            policy: Policy::Abort,
            backup_dir: &dir.join("bk"),
            cache_dir: &dir.join("cache"),
            stop: &stop,
        };
        execute(plan, &run, &mut State::default())
    }

    #[test]
    fn create_never_replaces_a_file_that_appeared_after_planning() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a");
        for desired in [
            Desired::Symlink("target".into()),
            Desired::File {
                hash: Hash::of(b"new"),
                meta: FileMeta::default(),
            },
        ] {
            let plan = create_plan(&dest, desired, Some(b"new"));
            fs::write(&dest, b"user data").unwrap();
            let report = run_plan(plan, dir.path());
            assert_eq!(report[0].action, Action::Fail, "{:?}", report[0].detail);
            assert_eq!(fs::read(&dest).unwrap(), b"user data");
            fs::remove_file(&dest).unwrap();
        }
    }
}
