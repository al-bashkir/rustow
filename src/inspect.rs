//! Read-only inspection from the same plan that `apply` executes:
//! `diff` (may print secrets by design), `status` and `check` (never print
//! content).

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::diff;
use crate::error::Result;
use crate::fsops::{self, FileMeta, Observed};
use crate::paths::RelPath;
use crate::plan::{Conflict, Decision, Deploy, Desired, Plan, PlanItem, Step};
use crate::repo::Context;
use crate::report::Printer;
use crate::source::{Discovery, Mode, check_collisions};
use crate::state::State;
use crate::template;

/// Prints differences; exit 0 when none, 1 when some, 4 when desired
/// content could not be computed for an item.
pub fn diff(plan: &Plan, printer: &Printer) -> Result<i32> {
    let (mut differs, mut failed) = (false, false);
    for item in &plan.items {
        let label = printer.path(&item.dest);
        let text = match &item.step {
            Step::Failed(msg) => {
                eprintln!("rustow: {}: cannot compute desired content: {msg}", item.source.path);
                failed = true;
                continue;
            }
            Step::Blocked { conflict, detail } => {
                format!("{label}: {}: {detail}\n", conflict.describe())
            }
            Step::Unfold { .. } => format!("{label}: folded directory link; would become a real directory\n"),
            Step::Deploy(d) => describe(&label, d)?,
        };
        if !text.is_empty() {
            differs = true;
            print!("{text}");
        }
    }
    Ok(if failed { 4 } else { i32::from(differs) })
}

fn describe(label: &str, d: &Deploy) -> Result<String> {
    let mut out = String::new();
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
            out.push_str(&diff::render(
                label,
                current.as_deref(),
                d.content.as_deref().map(Vec::as_slice),
            ));
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
    Ok(out)
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
    let mut rows: Vec<Row> = plan.items.iter().map(row).collect();
    rows.extend(orphans(ctx, discovery, state, args)?);
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

fn row(item: &PlanItem) -> Row {
    let (status, detail) = classify_item(item);
    Row {
        source: item.source.path.to_string(),
        dest: Some(item.dest.clone()),
        mode: item.source.mode.label().to_owned(),
        status,
        detail,
    }
}

fn classify_item(item: &PlanItem) -> (&'static str, Option<String>) {
    match &item.step {
        Step::Failed(msg) => ("ERROR", Some(msg.clone())),
        Step::Blocked { conflict, detail } => ("CONFLICT", Some(format!("{}: {detail}", conflict.describe()))),
        Step::Unfold { .. } => ("OUTDATED", Some("folded directory will be unfolded".into())),
        Step::Deploy(d) => classify_deploy(d, &item.source.mode),
    }
}

/// Status of a deploy step from its destination and cache decisions.
fn classify_deploy(d: &Deploy, mode: &Mode) -> (&'static str, Option<String>) {
    let cache = d.cache.as_ref().map(|c| c.decision);
    match (d.decision, cache) {
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
fn orphans(ctx: &Context, discovery: &Discovery, state: &State, args: &[String]) -> Result<Vec<Row>> {
    let prefixes: Vec<RelPath> = args
        .iter()
        .filter(|a| !matches!(a.as_str(), "." | "./"))
        .map(|a| RelPath::parse(a))
        .collect::<Result<_>>()?;
    let mut rows = Vec::new();
    for (dest, record) in &state.entries {
        let known = discovery.sources.iter().any(|s| s.path == record.source)
            || (record.folded_dir && ctx.root.join(record.source.as_path()).is_dir());
        let selected = prefixes.is_empty() || prefixes.iter().any(|p| record.source.starts_with(p));
        if known || !selected || !dest.starts_with(ctx.target_root(record.scope)) {
            continue;
        }
        rows.push(Row {
            source: record.source.to_string(),
            dest: Some(dest.clone()),
            mode: record.mode.clone(),
            status: "ORPHANED",
            detail: Some("source removed; `rustow undeploy` removes the destination".into()),
        });
    }
    Ok(rows)
}

/// Offline validation: discovery (configuration, ignore rules,
/// registrations), destination collisions and plaintext template
/// rendering. Never decrypts. Exit 0 clean, 1 findings.
pub fn check(ctx: &Context, printer: &Printer) -> Result<i32> {
    let discovery = ctx.discover()?;
    let mut findings: Vec<String> = check_collisions(&discovery.sources)
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
