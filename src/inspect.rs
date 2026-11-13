//! Read-only inspection from the same plan that `apply` executes:
//! `diff` (may print secrets by design), `status` and `check` (never print
//! content).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::config::CONFIG_FILE;
use crate::diff;
use crate::error::{IoContext, Result};
use crate::execute::{Action, ItemReport};
use crate::fsops::{self, FileMeta, Observed};
use crate::hash::Hash;
use crate::paths::RelPath;
use crate::plan::{Conflict, Content, Decision, Deploy, Desired, Plan, PlanItem, Step, resolve_id};
use crate::repo::Context;
use crate::report::{self, Printer, shown};
use crate::source::{Discovery, Mode, Selector, SkipKind, Skipped, check_collisions, shown_label};
use crate::state::{Record, State};
use crate::template;

/// The largest file that `diff` shows; a larger one gets a line with the sizes.
const DIFF_MAX: u64 = 1 << 20;

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
        print_json(&shown, &printer.warnings);
    } else {
        for s in &shown {
            match (s.kind.as_str(), s.detail.as_deref(), &s.item.step) {
                ("error", Some(msg), Step::Deploy(_)) => {
                    eprintln!("rustow: {}: {}", printer.path(&s.item.dest), report::shown(msg));
                }
                ("error", Some(msg), _) => eprintln!(
                    "rustow: {}: cannot compute desired content: {}",
                    report::shown(s.item.source.path.as_str()),
                    report::shown(msg)
                ),
                _ => crate::report::out(format_args!("{}", s.text)),
            }
        }
    }
    let failed = shown.iter().any(|s| s.kind == "error");
    Ok(if failed { 4 } else { i32::from(!shown.is_empty()) })
}

/// What `diff` shows for one item; `None` when the item does not differ.
/// A destination that the user cannot read and that would change is an
/// error of that item only.
fn show<'a>(item: &'a PlanItem, old: Option<&Record>, printer: &Printer) -> Result<Option<Shown<'a>>> {
    if let Step::Deploy(d) = &item.step
        && matches!(d.observed, Observed::Unreadable { .. })
        && d.decision != Decision::UpToDate
    {
        let why = "cannot read the destination (Permission denied); elevated privileges may be required";
        return Ok(Some(Shown {
            item,
            kind: "error".into(),
            detail: Some(why.into()),
            binary: false,
            text: String::new(),
        }));
    }
    let (text, binary) = step_text(item, &printer.path(&item.dest), old)?;
    let (status, detail) = match &item.step {
        // `diff` only reads: a directory it cannot write does not hide the content.
        Step::Deploy(d) => classify_deploy(d, &item.source.mode, old),
        _ => classify_item(item, old),
    };
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
        Step::Blocked { conflict, detail } => (format!("{label}: {conflict}: {}\n", shown(detail)), false),
        Step::Unfold { .. } => (
            format!("{label}: folded directory link; would become a real directory\n"),
            false,
        ),
        Step::Deploy(d) => deploy_text(label, d, old)?,
    })
}

/// One JSON object for each differing item, and the warnings. Holds content by design.
fn print_json(shown: &[Shown], warnings: &[serde_json::Value]) {
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
    outln!("{}", json!({ "items": items, "warnings": warnings }));
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
                    shown(&text.display().to_string())
                ));
            }
            if let Some(cache) = &d.cache {
                let (text, bin) =
                    content_diff(label, &cache.path, &cache.observed, (&cache.hash, Some(&cache.content)))?;
                binary |= bin;
                out.push_str(&text);
            }
        }
        Desired::File { meta, hash } => {
            if !matches!(d.observed, Observed::File { .. } | Observed::Missing) {
                out.push_str(&format!(
                    "{label}: {}; would become a regular file\n",
                    what(&d.observed)
                ));
            }
            let (text, bin) = content_diff(label, &d.path, &d.observed, (hash, d.content.as_ref()))?;
            binary |= bin;
            out.push_str(&text);
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

/// The diff of the current file at `path` (seen as `observed`) against the
/// wanted content, and whether it is binary. It reads content only when
/// the hashes differ, and only up to `DIFF_MAX` bytes on each side.
fn content_diff(
    label: &str,
    path: &Path,
    observed: &Observed,
    (hash, want): (&Hash, Option<&Content>),
) -> Result<(String, bool)> {
    let current = match observed {
        Observed::File { hash: have, .. } if have == hash => return Ok((String::new(), false)),
        Observed::File { .. } => Some(fs::symlink_metadata(path).ctx(|| format!("inspect {}", path.display()))?),
        _ => None,
    };
    let sizes = (
        current.as_ref().map_or(0, fs::Metadata::len),
        want.map_or(0, Content::size),
    );
    if sizes.0.max(sizes.1) > DIFF_MAX {
        let text = format!("{label}: differ ({} → {} bytes); too large to show\n", sizes.0, sizes.1);
        return Ok((text, false));
    }
    let current = current.map(|_| fsops::read_file(path)).transpose()?;
    let desired = want.map(Content::read).transpose()?;
    let (current, desired) = (current.as_deref(), desired.as_deref());
    Ok((diff::render(label, current, desired), diff::is_binary(current, desired)))
}

fn what(observed: &Observed) -> String {
    match observed {
        Observed::Missing => "missing".into(),
        Observed::Symlink(t) => format!("links to {}", shown(&t.display().to_string())),
        Observed::File { .. } => "regular file".into(),
        Observed::Unreadable { .. } => "regular file that you cannot read".into(),
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

/// Statuses that do not need attention. IGNORED and SKIPPED rows always show.
const CLEAN: [&str; 5] = ["LINKED", "COPIED", "TEMPLATED", "IGNORED", "SKIPPED"];

/// Status rows for the plan, recorded destinations whose source is gone,
/// skipped sources and (on request) ignored paths. Exit 0 when everything
/// is clean; a skipped source does not change it.
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
                mode: shown_label(&record.mode, record.backend),
                status: "ORPHANED",
                detail: Some(orphan_detail(ctx, discovery, record)),
            }),
    );
    // An inactive template that is already an ORPHANED row gets no second row.
    let orphaned: Vec<String> = rows
        .iter()
        .filter(|r| r.status == "ORPHANED")
        .map(|r| r.source.clone())
        .collect();
    let inactive = ctx
        .inactive(discovery, args)?
        .into_iter()
        .filter(|s| !orphaned.contains(&s.path.to_string()));
    rows.extend(ctx.skipped(discovery, args)?.into_iter().chain(inactive).map(|s| Row {
        source: s.path.to_string(),
        dest: None,
        mode: String::new(),
        status: "SKIPPED",
        detail: Some(s.reason.clone()),
    }));
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
        outln!("{}", json!({ "items": items }));
    } else {
        print_rows(&rows, attention, printer);
    }
    Ok(i32::from(attention > 0))
}

/// The status rows as text, then the summary line.
fn print_rows(rows: &[Row], attention: usize, printer: &Printer) {
    for r in rows
        .iter()
        .filter(|r| printer.verbose || !CLEAN.contains(&r.status) || matches!(r.status, "IGNORED" | "SKIPPED"))
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
        let detail = report::detail(r.detail.as_deref());
        outln!("{:>9} {dest}{}{mode}{detail}", r.status, shown(&r.source));
    }
    if !printer.quiet {
        outln!("{}", summary(rows, attention));
    }
}

/// "N ok, N need attention"; skipped sources and ignored paths are counted
/// apart, when any.
fn summary(rows: &[Row], attention: usize) -> String {
    let count = |status| rows.iter().filter(|r| r.status == status).count();
    let (skipped, ignored) = (count("SKIPPED"), count("IGNORED"));
    let mut parts = vec![format!("{} ok", rows.len() - attention - skipped - ignored)];
    for (n, what) in [(skipped, "skipped"), (ignored, "ignored")] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    parts.push(format!("{attention} need attention"));
    parts.join(", ")
}

fn row(item: &PlanItem, state: &State) -> Row {
    let (status, detail) = classify_item(item, state.entries.get(&item.dest));
    Row {
        source: item.source.path.to_string(),
        dest: Some(item.dest.clone()),
        mode: item.source.mode.shown_label(),
        status,
        detail,
    }
}

fn classify_item(item: &PlanItem, old: Option<&Record>) -> (&'static str, Option<String>) {
    match &item.step {
        Step::Failed(msg) => ("ERROR", Some(msg.clone())),
        Step::Deploy(d) if d.unwritable.is_some() => ("ERROR", d.unwritable.clone()),
        Step::Blocked { conflict, detail } => ("CONFLICT", Some(format!("{conflict}: {detail}"))),
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
        (Decision::Conflict(c), _) | (_, Some(Decision::Conflict(c))) => ("CONFLICT", Some(c.to_string())),
        (Decision::Create, _) => ("MISSING", None),
        (Decision::Update | Decision::FixMetadata, _) | (_, Some(Decision::Create | Decision::Update)) => {
            ("OUTDATED", None)
        }
        (Decision::Adopt, _) => (
            "UNMANAGED",
            Some("already correct but not recorded; apply adopts it".into()),
        ),
        // The detail of a destination that the plan compared with the state.
        _ => (deployed_status(mode), d.detail.clone()),
    }
}

/// Status of an up-to-date destination, by mode.
fn deployed_status(mode: &Mode) -> &'static str {
    match mode {
        Mode::Link | Mode::DecryptLink(_) => "LINKED",
        Mode::FileCopy(_) | Mode::DecryptCopy(..) => "COPIED",
        Mode::TemplateLink(_) | Mode::TemplateCopy(..) => "TEMPLATED",
    }
}

/// The recorded source is still in the repository: one of `known` (the
/// sources and skipped files) or a folded directory.
fn source_exists(ctx: &Context, known: &HashSet<&RelPath>, record: &Record) -> bool {
    known.contains(&record.source) || (record.folded_dir && ctx.root.join(record.source.as_path()).is_dir())
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
    let present = discovery.skipped.iter().filter(|s| s.kind == SkipKind::Present);
    let paths: HashSet<&RelPath> = (discovery.sources.iter().map(|s| &s.path))
        .chain(present.map(|s| &s.path))
        .collect();
    let mut found = Vec::new();
    for (dest, record) in &state.entries {
        let known = source_exists(ctx, &paths, record);
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
/// A template whose meta condition turned false names that condition; a
/// source that a `[[global_rules]]` entry ignores names the rule.
fn orphan_detail(ctx: &Context, discovery: &Discovery, record: &Record) -> String {
    let inactive = discovery.inactive.iter().find(|s| s.path == record.source);
    let why = match (inactive, ctx.ignore.ignoring_rule(&record.source, record.folded_dir)) {
        (Some(s), _) => s.reason.clone(),
        (None, Some(rule)) => format!("ignored by {rule}"),
        (None, None) => "source removed".into(),
    };
    format!("{why}; `rustow undeploy {}` removes it", record.source)
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
            mode: shown_label(&record.mode, record.backend),
            action: Action::Orphaned,
            detail: Some(orphan_detail(ctx, discovery, record)),
        })
        .collect())
}

/// Offline validation: discovery (configuration, ignore rules,
/// registrations), destination collisions, owner and group names and
/// plaintext template rendering. Never decrypts. Exit 0 clean, 1 findings.
/// Skipped sources are warnings, not findings, except a template whose
/// meta block is wrong: `apply` would quietly stop managing it.
pub fn check(ctx: &Context, printer: &mut Printer) -> Result<i32> {
    let discovery = ctx.discover()?;
    let (meta_problems, skipped): (Vec<&Skipped>, Vec<&Skipped>) =
        discovery.skipped.iter().partition(|s| s.is_meta_problem());
    printer.warn(skipped);
    printer.warn(&discovery.rule_warnings);
    let mut findings: Vec<String> = check_collisions(&discovery.sources, crate::plan::case_folding(ctx))
        .iter()
        .map(ToString::to_string)
        .collect();
    findings.extend(meta_problems.iter().map(|s| format!("{}: {}", s.path, s.reason)));
    findings.extend(unknown_ids(ctx));
    let untrusted = crate::plan::Conflict::UntrustedSource;
    findings.extend(
        discovery
            .sources
            .iter()
            .filter_map(|s| crate::plan::untrusted_source(ctx, s).map(|why| format!("{}: {untrusted}: {why}", s.path))),
    );
    let context = template::context(&ctx.config.vars, &ctx.home_target);
    for s in discovery
        .sources
        .iter()
        .filter(|s| matches!(s.mode, Mode::TemplateLink(None) | Mode::TemplateCopy(None, _)))
    {
        let bytes = fsops::read_file(&ctx.root.join(s.path.as_path()))?;
        let text = String::from_utf8_lossy(&bytes);
        if let Err(e) = template::render_plain(s.path.as_str(), &text, &context) {
            findings.push(e);
        }
    }
    if printer.json {
        outln!(
            "{}",
            json!({ "sources": discovery.sources.len(), "problems": findings, "warnings": printer.warnings })
        );
    } else {
        for f in &findings {
            outln!("problem: {}", shown(f));
        }
        if !printer.quiet {
            outln!(
                "{} sources checked, {} problems",
                discovery.sources.len(),
                findings.len()
            );
        }
    }
    Ok(i32::from(!findings.is_empty()))
}

/// Each `[[files]]` or `[[template_rules]]` owner or group that this system does not know.
fn unknown_ids(ctx: &Context) -> Vec<String> {
    let mut out = Vec::new();
    for (table, entries) in [
        ("files", &ctx.config.files),
        ("template_rules", &ctx.config.template_rules),
    ] {
        for (path, meta) in entries {
            for (id, group) in [(&meta.owner, false), (&meta.group, true)] {
                if let Some(Err(why)) = id.as_ref().map(|o| resolve_id(o, group)) {
                    out.push(format!("{CONFIG_FILE}: [[{table}]] source '{path}': {why}"));
                }
            }
        }
    }
    out
}
