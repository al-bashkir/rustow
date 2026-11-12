//! Read-only inspection from the same plan that `apply` executes:
//! `diff` (may print secrets by design), `status` and `check` (never print
//! content).

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::diff;
use crate::error::Result;
use crate::execute::{Action, ItemReport};
use crate::fsops::{self, FileMeta, Observed};
use crate::paths::RelPath;
use crate::plan::{Conflict, Decision, Deploy, Desired, Plan, PlanItem, Step};
use crate::repo::Context;
use crate::report::Printer;
use crate::source::{Discovery, Mode, Selector, check_collisions};
use crate::state::{Record, State};
use crate::template;

/// One differing item, as `diff` shows it.
struct Shown<'a> {
    item: &'a PlanItem,
    /// The status word in lowercase (`missing`, `outdated`, `modified`,
    /// `conflict`, `error`, ...), or `metadata` when only metadata differs.
    kind: String,
    detail: Option<String>,
    binary: bool,
    text: String,
}

/// Prints differences, as text or as JSON; exit 0 when none, 1 when some,
/// 4 when desired content could not be computed for an item.
pub fn diff(plan: &Plan, state: &State, printer: &Printer) -> Result<i32> {
    let mut shown = Vec::new();
    for item in &plan.items {
        shown.extend(show(item, state.entries.get(&item.dest), printer)?);
    }
    if printer.json {
        print_json(&shown);
    } else {
        for s in &shown {
            match (s.kind.as_str(), s.detail.as_deref()) {
                ("error", Some(msg)) => {
                    eprintln!("rustow: {}: cannot compute desired content: {msg}", s.item.source.path)
                }
                _ => print!("{}", s.text),
            }
        }
    }
    let failed = shown.iter().any(|s| s.kind == "error");
    Ok(if failed { 4 } else { i32::from(!shown.is_empty()) })
}

/// What `diff` shows for one item; `None` when the item does not differ.
fn show<'a>(item: &'a PlanItem, old: Option<&Record>, printer: &Printer) -> Result<Option<Shown<'a>>> {
    let (text, binary) = step_text(item, &printer.path(&item.dest), old)?;
    let (status, detail) = classify_item(item, old);
    if text.is_empty() && status != "ERROR" {
        return Ok(None);
    }
    let kind = match &item.step {
        Step::Deploy(d) if d.decision == Decision::FixMetadata => "metadata".into(),
        _ => status.to_lowercase(),
    };
    Ok(Some(Shown {
        item,
        kind,
        detail,
        binary,
        text,
    }))
}

/// Diff text of one item, and whether it is binary. Empty for a failed item.
fn step_text(item: &PlanItem, label: &str, old: Option<&Record>) -> Result<(String, bool)> {
    Ok(match &item.step {
        Step::Failed(_) => (String::new(), false),
        Step::Blocked { conflict, detail } => (format!("{label}: {}: {detail}\n", conflict.describe()), false),
        Step::Unfold { .. } => (
            format!("{label}: folded directory link; would become a real directory\n"),
            false,
        ),
        Step::Deploy(d) => deploy_text(label, d, old)?,
    })
}

/// One JSON object for each differing item. Holds content by design.
fn print_json(shown: &[Shown]) {
    let items: Vec<_> = shown
        .iter()
        .map(|s| {
            json!({
                "dest": s.item.dest,
                "source": s.item.source.path.as_str(),
                "mode": s.item.source.mode.label(),
                "kind": s.kind,
                "detail": s.detail,
                "binary": s.binary,
                "diff": s.text,
            })
        })
        .collect();
    println!("{}", json!({ "items": items }));
}

/// Diff text of a deploy step, and whether it is binary; marks a
/// destination whose source changed too.
fn deploy_text(label: &str, d: &Deploy, old: Option<&Record>) -> Result<(String, bool)> {
    let (text, binary) = describe(label, d)?;
    if both_changed(d, old) {
        return Ok((format!("{label}: conflict: {BOTH_CHANGED}\n{text}"), binary));
    }
    Ok((text, binary))
}

fn describe(label: &str, d: &Deploy) -> Result<(String, bool)> {
    let mut out = String::new();
    let mut binary = false;
    match &d.desired {
        Desired::Symlink(text) => {
            if !matches!(&d.observed, Observed::Symlink(t) if t == text) {
                out.push_str(&format!(
                    "{label}: {}; would link to {}\n",
                    what(&d.observed),
                    text.display()
                ));
            }
            if let Some(cache) = &d.cache {
                let current = current_bytes(&cache.path, &cache.observed)?;
                binary |= diff::is_binary(current.as_deref(), Some(&cache.bytes));
                out.push_str(&diff::render(label, current.as_deref(), Some(&cache.bytes)));
            }
        }
        Desired::File { meta, .. } => {
            if !matches!(d.observed, Observed::File { .. } | Observed::Missing) {
                out.push_str(&format!(
                    "{label}: {}; would become a regular file\n",
                    what(&d.observed)
                ));
            }
            let current = current_bytes(&d.path, &d.observed)?;
            let desired = d.content.as_deref().map(Vec::as_slice);
            binary |= diff::is_binary(current.as_deref(), desired);
            out.push_str(&diff::render(label, current.as_deref(), desired));
            if let Observed::File { mode, uid, gid, .. } = d.observed
                && !meta.matches(mode, uid, gid)
            {
                out.push_str(&format!(
                    "{label}: metadata {} -> {}\n",
                    show_meta(mode, uid, gid),
                    show_wanted(meta)
                ));
            }
        }
    }
    Ok((out, binary))
}

fn current_bytes(path: &Path, observed: &Observed) -> Result<Option<Vec<u8>>> {
    match observed {
        Observed::File { .. } => fsops::read_file(path).map(Some),
        _ => Ok(None),
    }
}

fn what(observed: &Observed) -> String {
    match observed {
        Observed::Missing => "missing".into(),
        Observed::Symlink(t) => format!("links to {}", t.display()),
        Observed::File { .. } => "regular file".into(),
        Observed::Dir => "directory".into(),
        Observed::Other => "special file".into(),
    }
}

fn show_meta(mode: u32, uid: u32, gid: u32) -> String {
    format!("mode {mode:04o} uid {uid} gid {gid}")
}

fn show_wanted(meta: &FileMeta) -> String {
    let mut parts = Vec::new();
    if let Some(m) = meta.mode {
        parts.push(format!("mode {m:04o}"));
    }
    if let Some(u) = meta.uid {
        parts.push(format!("uid {u}"));
    }
    if let Some(g) = meta.gid {
        parts.push(format!("gid {g}"));
    }
    parts.join(" ")
}

/// One status line.
pub struct Row {
    pub source: String,
    pub dest: Option<PathBuf>,
    pub mode: String,
    pub status: &'static str,
    pub detail: Option<String>,
}

const CLEAN: [&str; 4] = ["LINKED", "COPIED", "TEMPLATED", "IGNORED"];

/// Status rows for the plan, recorded destinations whose source is gone,
/// and (on request) ignored paths. Exit 0 when everything is clean.
pub fn status(
    ctx: &Context,
    plan: &Plan,
    discovery: &Discovery,
    state: &State,
    args: &[String],
    ignored: bool,
    printer: &Printer,
) -> Result<i32> {
    let mut rows: Vec<Row> = plan.items.iter().map(|i| row(i, state)).collect();
    rows.extend(
        orphans(ctx, discovery, state, args)?
            .into_iter()
            .map(|(dest, record)| Row {
                source: record.source.to_string(),
                dest: Some(dest.clone()),
                mode: record.mode.clone(),
                status: "ORPHANED",
                detail: Some(orphan_detail(record)),
            }),
    );
    if ignored {
        rows.extend(discovery.ignored.iter().map(|p| Row {
            source: p.to_string(),
            dest: None,
            mode: String::new(),
            status: "IGNORED",
            detail: None,
        }));
    }
    let attention = rows.iter().filter(|r| !CLEAN.contains(&r.status)).count();
    if printer.json {
        let items: Vec<_> = rows
            .iter()
            .map(|r| json!({ "source": r.source, "dest": r.dest, "mode": r.mode, "status": r.status, "detail": r.detail }))
            .collect();
        println!("{}", json!({ "items": items }));
    } else {
        for r in rows
            .iter()
            .filter(|r| printer.verbose || !CLEAN.contains(&r.status) || r.status == "IGNORED")
        {
            let dest = r
                .dest
                .as_deref()
                .map(|d| format!("{}  <- ", printer.path(d)))
                .unwrap_or_default();
            let mode = if r.mode.is_empty() {
                String::new()
            } else {
                format!(" ({})", r.mode)
            };
            let detail = r.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default();
            println!("{:>9} {dest}{}{mode}{detail}", r.status, r.source);
        }
        if !printer.quiet {
            println!("{} ok, {attention} need attention", rows.len() - attention);
        }
    }
    Ok(i32::from(attention > 0))
}

fn row(item: &PlanItem, state: &State) -> Row {
    let (status, detail) = classify_item(item, state.entries.get(&item.dest));
    Row {
        source: item.source.path.to_string(),
        dest: Some(item.dest.clone()),
        mode: item.source.mode.label().to_owned(),
        status,
        detail,
    }
}

fn classify_item(item: &PlanItem, old: Option<&Record>) -> (&'static str, Option<String>) {
    match &item.step {
        Step::Failed(msg) => ("ERROR", Some(msg.clone())),
        Step::Blocked { conflict, detail } => ("CONFLICT", Some(format!("{}: {detail}", conflict.describe()))),
        Step::Unfold { .. } => ("OUTDATED", Some("folded directory will be unfolded".into())),
        Step::Deploy(d) => classify_deploy(d, &item.source.mode, old),
    }
}

const BOTH_CHANGED: &str = "source and destination both changed since rustow last applied it";

/// A modified destination (or cache entry) whose desired content changed too
/// since the last apply (spec §8.5). `old` is the record of that apply.
fn both_changed(d: &Deploy, old: Option<&Record>) -> bool {
    let modified = Decision::Conflict(Conflict::Modified);
    (d.decision == modified || d.cache.as_ref().is_some_and(|c| c.decision == modified))
        && old.is_some_and(|r| r.applied_hash != d.record.applied_hash)
}

/// Status of a deploy step from its destination and cache decisions.
fn classify_deploy(d: &Deploy, mode: &Mode, old: Option<&Record>) -> (&'static str, Option<String>) {
    let cache = d.cache.as_ref().map(|c| c.decision);
    match (d.decision, cache) {
        _ if both_changed(d, old) => ("CONFLICT", Some(BOTH_CHANGED.into())),
        (Decision::Conflict(Conflict::Modified), _) | (_, Some(Decision::Conflict(Conflict::Modified))) => {
            ("MODIFIED", None)
        }
        (Decision::Conflict(c), _) | (_, Some(Decision::Conflict(c))) => ("CONFLICT", Some(c.describe().into())),
        (Decision::Create, _) => ("MISSING", None),
        (Decision::Update | Decision::FixMetadata, _) | (_, Some(Decision::Create | Decision::Update)) => {
            ("OUTDATED", None)
        }
        (Decision::Adopt, _) => (
            "UNMANAGED",
            Some("already correct but not recorded; apply adopts it".into()),
        ),
        _ => (deployed_status(mode), None),
    }
}

/// Status of an up-to-date destination, by mode.
fn deployed_status(mode: &Mode) -> &'static str {
    match mode {
        Mode::Link | Mode::DecryptLink(_) => "LINKED",
        Mode::FileCopy | Mode::DecryptCopy(_) => "COPIED",
        Mode::TemplateLink(_) | Mode::TemplateCopy(..) => "TEMPLATED",
    }
}

/// Recorded destinations in the current targets whose source no longer exists.
fn orphans<'a>(
    ctx: &Context,
    discovery: &Discovery,
    state: &'a State,
    args: &[String],
) -> Result<Vec<(&'a PathBuf, &'a Record)>> {
    let prefixes: Vec<RelPath> = args
        .iter()
        .filter(|a| !matches!(a.as_str(), "." | "./"))
        .map(|a| RelPath::parse(a))
        .collect::<Result<_>>()?;
    let mut found = Vec::new();
    for (dest, record) in &state.entries {
        let known = discovery.sources.iter().any(|s| s.path == record.source)
            || (record.folded_dir && ctx.root.join(record.source.as_path()).is_dir());
        let selected = prefixes.is_empty() || prefixes.iter().any(|p| record.source.starts_with(p));
        if known || !selected || !dest.starts_with(ctx.target_root(record.scope)) {
            continue;
        }
        found.push((dest, record));
    }
    Ok(found)
}

/// Names the command that removes one orphaned destination (`undeploy`
/// with no path would remove every managed destination).
fn orphan_detail(record: &Record) -> String {
    format!("source removed; `rustow undeploy {}` removes it", record.source)
}

/// Report lines for the orphaned destinations a deploy command covers.
/// Nothing is removed and the exit code does not change.
pub fn orphan_reports(
    ctx: &Context,
    discovery: &Discovery,
    state: &State,
    args: &[String],
    selector: Selector,
) -> Result<Vec<ItemReport>> {
    let found = orphans(ctx, discovery, state, args)?;
    Ok(found
        .into_iter()
        .filter(|(_, record)| selector.includes_label(&record.mode))
        .map(|(dest, record)| ItemReport {
            source: record.source.clone(),
            dest: dest.clone(),
            mode: record.mode.clone(),
            action: Action::Orphaned,
            detail: Some(orphan_detail(record)),
        })
        .collect())
}

/// Offline validation: discovery (configuration, ignore rules,
/// registrations), destination collisions and plaintext template
/// rendering. Never decrypts. Exit 0 clean, 1 findings.
pub fn check(ctx: &Context, printer: &Printer) -> Result<i32> {
    let discovery = ctx.discover()?;
    let mut findings: Vec<String> = check_collisions(&discovery.sources, crate::plan::case_folding(ctx))
        .iter()
        .map(ToString::to_string)
        .collect();
    let context = template::context(&ctx.config.vars, &ctx.home_target);
    for s in discovery
        .sources
        .iter()
        .filter(|s| matches!(s.mode, Mode::TemplateLink(None) | Mode::TemplateCopy(None, _)))
    {
        let bytes = fsops::read_file(&ctx.root.join(s.path.as_path()))?;
        let text = String::from_utf8_lossy(&bytes);
        if let Err(e) = template::render(s.path.as_str(), &text, &context) {
            findings.push(e);
        }
    }
    if printer.json {
        println!(
            "{}",
            json!({ "sources": discovery.sources.len(), "problems": findings })
        );
    } else {
        for f in &findings {
            println!("problem: {f}");
        }
        if !printer.quiet {
            println!(
                "{} sources checked, {} problems",
                discovery.sources.len(),
                findings.len()
            );
        }
    }
    Ok(i32::from(!findings.is_empty()))
}
