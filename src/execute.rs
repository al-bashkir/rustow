//! Runs a plan. Each item is applied, verified and recorded on its own,
//! so a failure leaves earlier items deployed and recorded and later
//! items untouched. There is no global rollback.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::fsops::{self, Observed};
use crate::paths::RelPath;
use crate::plan::{Conflict, Decision, Deploy, Desired, Plan, PlanItem, Removal, Step, Verdict};
use crate::state::State;
use crate::sudo::Job;

/// What to do with a resolvable conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
pub enum Policy {
    /// Change nothing at all when any item conflicts.
    Abort,
    /// Leave conflicting destinations as they are; deploy the rest (default).
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
    /// Report only: a managed destination whose source was removed.
    Orphaned,
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
            Action::Orphaned => "orphaned",
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

/// `predict_local`, with "via sudo" on a change that the sudo helper makes.
fn predict_deploy(d: &Deploy, policy: Policy) -> (Action, Option<String>) {
    let (action, detail) = predict_local(d, policy);
    if d.sudo
        && matches!(
            action,
            Action::Create | Action::Update | Action::Replace | Action::Metadata
        )
    {
        let detail = detail.map_or_else(|| "via sudo".to_owned(), |d| format!("{d}; via sudo"));
        return (action, Some(detail));
    }
    (action, detail)
}

fn predict_local(d: &Deploy, policy: Policy) -> (Action, Option<String>) {
    if let Some(why) = &d.unwritable {
        return (Action::Fail, Some(why.clone()));
    }
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
/// The `sudo` deploys wait for one sudo helper call at the end.
pub fn execute(plan: Plan, run: &Run, state: &mut State) -> Vec<ItemReport> {
    let mut out = Vec::with_capacity(plan.items.len());
    let mut queued = Vec::new();
    for item in plan.items {
        let (action, detail, sudo) = execute_item(&item, run, state);
        out.push(ItemReport {
            source: item.source.path.clone(),
            dest: item.dest.clone(),
            mode: item.source.mode.shown_label(),
            action,
            detail,
        });
        if sudo {
            queued.push((out.len() - 1, item));
        }
    }
    if !queued.is_empty() {
        run_sudo(&queued, run, state, &mut out);
    }
    out
}

/// Executes one item. For a `sudo` deploy it writes only the cache entry;
/// the bool says that the destination waits for the sudo helper.
fn execute_item(item: &PlanItem, run: &Run, state: &mut State) -> (Action, Option<String>, bool) {
    let (action, detail) = predict(item, run.policy);
    if run.stop.load(Ordering::SeqCst) {
        let why = "interrupted before this item; nothing changed";
        return (Action::Fail, Some(why.into()), false);
    }
    let (done, sudo) = match &item.step {
        Step::Deploy(_) if matches!(action, Action::Conflict | Action::Skip | Action::Fail) => {
            return (action, detail, false);
        }
        Step::Deploy(d) if d.sudo => (apply_cache(d, item, run), true),
        Step::Deploy(d) => (apply(d, item, run, state), false),
        Step::Unfold { path, observed } => (unfold(path, observed, &item.dest, state).map(|()| None), false),
        Step::Blocked { .. } | Step::Failed(_) => return (action, detail, false),
    };
    match done {
        Ok(extra) => (action, extra.or(detail), sudo),
        Err(e) => (Action::Fail, Some(crate::plan::with_privilege_hint(e)), false),
    }
}

/// Writes the queued destinations through one sudo helper call, then
/// records each success and reports each failure.
fn run_sudo(queued: &[(usize, PlanItem)], run: &Run, state: &mut State, out: &mut [ItemReport]) {
    let deploys: Vec<(usize, &PlanItem, &Deploy)> = queued
        .iter()
        .filter_map(|(index, item)| match &item.step {
            Step::Deploy(d) => Some((*index, item, &**d)),
            _ => None,
        })
        .collect();
    let jobs: Vec<Job<&Deploy>> = deploys
        .iter()
        .map(|(_, item, d)| Job::Write {
            deploy: *d,
            policy: run.policy,
            backup: backup_path(run, item, false),
        })
        .collect();
    let results = sudo_batch(&jobs, run.stop, state);
    for ((index, item, d), result) in deploys.into_iter().zip(results) {
        sudo_report(&mut out[index], result, || {
            state.entries.insert(item.dest.clone(), d.record.clone());
        });
    }
}

/// Runs the jobs through one sudo helper call and applies the directories
/// it created or removed to `state`. A failed call fails every job.
fn sudo_batch(
    jobs: &[Job<&Deploy>],
    stop: &AtomicBool,
    state: &mut State,
) -> Vec<std::result::Result<Option<String>, String>> {
    let done = if stop.load(Ordering::SeqCst) {
        Err(Error::Failed("interrupted before sudo; nothing changed".into()))
    } else {
        crate::sudo::call(jobs)
    };
    match done {
        Ok(done) => done
            .into_iter()
            .map(|d| {
                state.created_dirs.extend(d.created_dirs);
                for dir in &d.removed_dirs {
                    state.created_dirs.remove(dir);
                }
                d.result
            })
            .collect(),
        Err(e) => vec![Err(e.to_string()); jobs.len()],
    }
}

/// Puts one helper result into its report; `record` runs on success.
fn sudo_report(report: &mut ItemReport, result: std::result::Result<Option<String>, String>, record: impl FnOnce()) {
    match result {
        Ok(extra) => {
            record();
            report.detail = extra.or(report.detail.take());
        }
        Err(why) => (report.action, report.detail) = (Action::Fail, Some(why)),
    }
}

/// The cache entry first, so that a link to it never dangles; then the destination.
fn apply(d: &Deploy, item: &PlanItem, run: &Run, state: &mut State) -> Result<Option<String>> {
    let cached = apply_cache(d, item, run)?;
    let mut created = Vec::new();
    let written = write_dest(d, run.policy, &backup_path(run, item, false), &mut created);
    state.created_dirs.extend(created);
    let extra = written?;
    state.entries.insert(item.dest.clone(), d.record.clone());
    Ok(extra.or(cached))
}

/// The destination part of a deploy: the missing directories (pushed to
/// `created` as they appear), the planned change and the check after it.
/// The sudo helper runs it as root.
pub fn write_dest(d: &Deploy, policy: Policy, backup: &Path, created: &mut Vec<PathBuf>) -> Result<Option<String>> {
    if let Some((existing, missing)) = &d.missing_dirs {
        created.extend(fsops::create_dirs(existing, missing, d.dir_mode)?);
    }
    let detail = apply_dest(d, policy, backup)?;
    verify(d)?;
    Ok(detail)
}

/// Writes the cache entry when it changes; returns the backup note, if any.
fn apply_cache(d: &Deploy, item: &PlanItem, run: &Run) -> Result<Option<String>> {
    let Some(cache) = &d.cache else { return Ok(None) };
    if matches!(cache.decision, Decision::UpToDate | Decision::Adopt) {
        return Ok(None);
    }
    fsops::ensure_private_tree(run.cache_dir, cache.path.parent().unwrap_or(run.cache_dir))?;
    recheck(&cache.path, &cache.observed)?;
    let mut detail = None;
    if let (Decision::Conflict(_), Policy::Backup) = (cache.decision, run.policy) {
        detail = Some(backup(&cache.path, &backup_path(run, item, true))?);
    }
    fsops::write_atomic(&cache.path, &cache.bytes, 0o600, None, None)?;
    Ok(detail)
}

/// Performs the planned destination decision (the caller already checked
/// that the policy allows it); returns the backup note, if any.
fn apply_dest(d: &Deploy, policy: Policy, backup: &Path) -> Result<Option<String>> {
    match d.decision {
        Decision::Create => create(d).map(|()| None),
        Decision::Update => update(d).map(|()| None),
        Decision::Conflict(_) => replace_conflicting(d, policy, backup),
        Decision::FixMetadata => fix_metadata(d).map(|()| None),
        Decision::UpToDate | Decision::Adopt => Ok(None),
    }
}

/// Replaces rustow's own unchanged object at the destination.
fn update(d: &Deploy) -> Result<()> {
    recheck(&d.path, &d.observed)?;
    place(d)
}

/// Moves away (backup) or removes the conflicting object, then creates.
fn replace_conflicting(d: &Deploy, policy: Policy, backup: &Path) -> Result<Option<String>> {
    recheck(&d.path, &d.observed)?;
    let detail = clear_conflict(d, policy, backup)?;
    create(d)?;
    Ok(detail)
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
fn clear_conflict(d: &Deploy, policy: Policy, to: &Path) -> Result<Option<String>> {
    if policy == Policy::Backup {
        return backup(&d.path, to).map(Some);
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
/// The `sudo` removals wait for one sudo helper call at the end.
pub fn remove(removals: Vec<Removal>, stop: &AtomicBool, state: &mut State) -> Vec<ItemReport> {
    let mut out = Vec::with_capacity(removals.len());
    let (mut jobs, mut slots) = (Vec::new(), Vec::new());
    for r in removals {
        let (action, detail, job) = remove_item(&r, stop, state);
        if let Some(job) = job {
            jobs.push(job);
            slots.push((out.len(), r.dest.clone()));
        }
        out.push(ItemReport {
            source: r.source,
            dest: r.dest,
            mode: r.mode,
            action,
            detail,
        });
    }
    if !jobs.is_empty() {
        let results = sudo_batch(&jobs, stop, state);
        for ((index, dest), result) in slots.into_iter().zip(results) {
            sudo_report(&mut out[index], result, || {
                state.entries.remove(&dest);
            });
        }
    }
    out
}

/// Removes one item, or returns the sudo job that removes it.
fn remove_item(
    r: &Removal,
    stop: &AtomicBool,
    state: &mut State,
) -> (Action, Option<String>, Option<Job<&'static Deploy>>) {
    if stop.load(Ordering::SeqCst) {
        let why = "interrupted before this item; nothing changed";
        return (Action::Fail, Some(why.into()), None);
    }
    match &r.verdict {
        Verdict::Keep(why) => (Action::Conflict, Some(why.clone()), None),
        Verdict::Forget => {
            state.entries.remove(&r.dest);
            (Action::Forget, Some("already gone".into()), None)
        }
        Verdict::Remove(observed) if r.sudo => {
            let created_dirs = r
                .path
                .ancestors()
                .skip(1)
                .take_while(|d| state.created_dirs.contains(*d))
                .map(Path::to_path_buf)
                .collect();
            let job = Job::Remove {
                path: r.path.clone(),
                observed: observed.clone(),
                created_dirs,
            };
            (Action::Remove, Some("via sudo".into()), Some(job))
        }
        Verdict::Remove(observed) => match remove_one(&r.path, observed, state) {
            Ok(()) => {
                state.entries.remove(&r.dest);
                (Action::Remove, None, None)
            }
            Err(e) => (Action::Fail, Some(e.to_string()), None),
        },
    }
}

fn remove_one(path: &Path, observed: &Observed, state: &mut State) -> Result<()> {
    let mut removed = Vec::new();
    let done = remove_dest(path, observed, &state.created_dirs, &mut removed);
    for dir in &removed {
        state.created_dirs.remove(dir);
    }
    done
}

/// Removes `path`, which must still look like `observed`, then each empty
/// directory above it that `created` lists (pushed to `removed`). The sudo
/// helper runs it as root.
pub fn remove_dest(
    path: &Path,
    observed: &Observed,
    created: &BTreeSet<PathBuf>,
    removed: &mut Vec<PathBuf>,
) -> Result<()> {
    recheck(path, observed)?;
    fs::remove_file(path).map_err(|e| Error::Failed(format!("remove {}: {e}", path.display())))?;
    for dir in path.ancestors().skip(1) {
        if !created.contains(dir) || fs::remove_dir(dir).is_err() {
            break;
        }
        removed.push(dir.to_path_buf());
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
                Verdict::Remove(_) => (Action::Remove, r.sudo.then(|| "via sudo".to_owned())),
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
            unwritable: None,
            sudo: false,
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
