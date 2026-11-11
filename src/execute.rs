//! Runs a plan. Each item is applied, verified and recorded on its own,
//! so a failure leaves earlier items deployed and recorded and later
//! items untouched. There is no global rollback.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::fsops::{self, Observed};
use crate::paths::RelPath;
use crate::plan::{Decision, Deploy, Desired, Plan, PlanItem, Removal, Step, Verdict};
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
    let unresolved = |detail: String| {
        let action = if policy == Policy::Skip {
            Action::Skip
        } else {
            Action::Conflict
        };
        (action, Some(detail))
    };
    match &item.step {
        Step::Failed(msg) => (Action::Fail, Some(msg.clone())),
        Step::Blocked { conflict, detail } => unresolved(format!("{}: {detail}", conflict.describe())),
        Step::Deploy(d) => {
            let cache_conflict = d.cache.as_ref().and_then(|c| match c.decision {
                Decision::Conflict(conflict) => Some(conflict),
                _ => None,
            });
            let dest_conflict = match d.decision {
                Decision::Conflict(conflict) => Some(conflict),
                _ => None,
            };
            if let Some(conflict) = dest_conflict.or(cache_conflict) {
                let what = if dest_conflict.is_some() { "" } else { "cache entry: " };
                let mut detail = format!("{what}{}", conflict.describe());
                if let Some(extra) = &d.detail {
                    detail = format!("{detail} ({extra})");
                }
                if conflict.resolvable() && matches!(policy, Policy::Backup | Policy::Overwrite) {
                    return (Action::Replace, Some(detail));
                }
                return unresolved(detail);
            }
            let cache_changes = d
                .cache
                .as_ref()
                .is_some_and(|c| c.decision != Decision::UpToDate && c.decision != Decision::Adopt);
            let action = match d.decision {
                Decision::Create => Action::Create,
                Decision::Update => Action::Update,
                Decision::FixMetadata => Action::Metadata,
                _ if cache_changes => Action::Update,
                Decision::Adopt => Action::Adopt,
                _ => Action::Unchanged,
            };
            (action, d.detail.clone())
        }
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
            match apply(d, action, &item, run, state) {
                Ok(Some(extra)) => detail = Some(extra),
                Ok(None) => {}
                Err(e) => (action, detail) = (Action::Fail, Some(e.to_string())),
            }
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

fn apply(d: &Deploy, action: Action, item: &PlanItem, run: &Run, state: &mut State) -> Result<Option<String>> {
    if let Some((existing, missing)) = &d.missing_dirs {
        state
            .created_dirs
            .extend(fsops::create_dirs(existing, missing, d.dir_mode)?);
    }
    let mut detail = None;
    if let Some(cache) = &d.cache
        && !matches!(cache.decision, Decision::UpToDate | Decision::Adopt)
    {
        fsops::ensure_private_dir(run.cache_dir)?;
        recheck(&cache.path, &cache.observed)?;
        if let (Decision::Conflict(_), Policy::Backup) = (cache.decision, run.policy) {
            detail = Some(backup(&cache.path, &backup_path(run, item, true))?);
        }
        if let Some(parent) = cache.path.parent() {
            fsops::ensure_private_dir(parent)?;
        }
        fsops::write_atomic(&cache.path, &cache.bytes, 0o600, None, None)?;
    }
    let dest_conflict = matches!(d.decision, Decision::Conflict(_));
    match action {
        Action::Create => place(d)?,
        Action::Update | Action::Replace if d.decision == Decision::Update || dest_conflict => {
            recheck(&d.path, &d.observed)?;
            if dest_conflict {
                match run.policy {
                    Policy::Backup => detail = Some(backup(&d.path, &backup_path(run, item, false))?),
                    _ => fs::remove_file(&d.path)
                        .map_err(|e| Error::Failed(format!("remove {}: {e}", d.path.display())))?,
                }
            }
            place(d)?;
        }
        Action::Metadata => {
            recheck(&d.path, &d.observed)?;
            if let Desired::File { meta, .. } = &d.desired {
                fsops::set_meta(&d.path, *meta)?;
            }
        }
        _ => {}
    }
    verify(d)?;
    state.entries.insert(item.dest.clone(), d.record.clone());
    Ok(detail)
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
            fsops::write_atomic(&d.path, bytes, meta.mode.unwrap_or(d.file_mode), meta.uid, meta.gid)
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
