//! Human and JSON output. Never prints file contents.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::execute::{Action, ItemReport};
use crate::source::Skipped;

/// Writes command results to stdout. rustow prints only after its work is
/// done, so a reader that closed the pipe (`rustow status | head`) costs only
/// output: exit at once with 141, as a shell reports SIGPIPE. SIGPIPE itself
/// stays ignored, so a gpg child that exits early cannot kill rustow mid-write.
pub fn out(args: std::fmt::Arguments) {
    use std::io::Write;
    if let Err(e) = std::io::stdout().lock().write_fmt(args) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(141);
        }
        eprintln!("rustow: error: write output: {e}");
        std::process::exit(4);
    }
}

/// Whether the reported items were applied or only planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Applied,
    DryRun,
    /// A real run that the conflict policy stopped before any change.
    Blocked,
}

pub struct Printer {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    color: bool,
    /// Shown as `~` in paths.
    home: Option<PathBuf>,
    /// The skipped sources of this run, as JSON `{source, reason}` objects.
    pub warnings: Vec<serde_json::Value>,
}

impl Printer {
    pub fn new(json: bool, quiet: bool, verbose: bool, no_color: bool) -> Printer {
        let color = !no_color && !json && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
        Printer {
            json,
            quiet,
            verbose,
            color,
            home: None,
            warnings: Vec::new(),
        }
    }

    pub fn set_home(&mut self, home: &Path) {
        self.home = Some(home.to_path_buf());
    }

    /// Informational message on stderr.
    pub fn notice(&self, msg: &str) {
        if !self.quiet && !self.json {
            eprintln!("note: {msg}");
        }
    }

    /// Prints one warning on stderr for each skipped source and keeps
    /// them for the JSON output. Warnings never change the exit code.
    pub fn warn<'a>(&mut self, skipped: impl IntoIterator<Item = &'a Skipped>) {
        for s in skipped {
            if !self.quiet && !self.json {
                eprintln!("warning: {}: {}", s.path, s.reason);
            }
            self.warnings
                .push(json!({ "source": s.path.as_str(), "reason": s.reason }));
        }
    }

    /// Path for display, with the home directory shortened to `~`.
    pub fn path(&self, path: &Path) -> String {
        match self.home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
            Some(rest) if rest.as_os_str().is_empty() => "~".into(),
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }

    pub fn items(&self, items: &[ItemReport], run: RunKind) {
        if self.json {
            let list: Vec<_> = items
                .iter()
                .map(|i| {
                    json!({
                        "source": i.source.as_str(),
                        "dest": i.dest,
                        "mode": i.mode,
                        "action": i.action.as_str(),
                        "detail": i.detail,
                    })
                })
                .collect();
            let out = json!({
                "dry_run": run == RunKind::DryRun,
                "aborted": run == RunKind::Blocked,
                "items": list,
                "summary": summary_counts(items),
                "warnings": self.warnings,
            });
            outln!("{out}");
            return;
        }
        let (problems, rest): (Vec<&ItemReport>, Vec<&ItemReport>) = items
            .iter()
            .partition(|i| matches!(i.action, Action::Conflict | Action::Skip | Action::Fail));
        for i in rest {
            if !self.quiet && (i.action != Action::Unchanged || self.verbose) {
                self.item_line(i);
            }
        }
        if !problems.is_empty() {
            outln!("Conflicts:");
            for i in problems {
                self.item_line(i);
            }
        }
        if !self.quiet {
            outln!("{}", summary_line(items, run));
        }
    }

    /// Prints per-file changes of init and file registration commands.
    pub fn changes(&self, repository: Option<&Path>, changes: &[Change], dry_run: bool) {
        if self.json {
            let list: Vec<_> = changes
                .iter()
                .map(|c| json!({ "path": c.path, "action": c.action, "detail": c.detail }))
                .collect();
            outln!(
                "{}",
                json!({ "repository": repository, "dry_run": dry_run, "changes": list })
            );
            return;
        }
        if let (false, Some(root)) = (self.quiet, repository) {
            outln!("repository: {}", root.display());
        }
        let mut changed = 0;
        for c in changes {
            changed += usize::from(c.action != "ok");
            if !self.quiet && (c.action != "ok" || self.verbose || c.detail.is_some()) {
                let detail = c.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default();
                outln!("{:>8} {}{detail}", c.action, c.path);
            }
        }
        if !self.quiet {
            let line = format!("{changed} changed, {} unchanged", changes.len() - changed);
            outln!(
                "{}",
                if dry_run {
                    format!("dry run, nothing written: {line}")
                } else {
                    line
                }
            );
        }
    }

    fn item_line(&self, i: &ItemReport) {
        let detail = i.detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default();
        outln!(
            "{} {}  <- {} ({}){detail}",
            self.verb(i.action),
            self.path(&i.dest),
            i.source,
            i.mode
        );
    }

    fn verb(&self, action: Action) -> String {
        let text = format!("{:>8}", action.as_str());
        if !self.color {
            return text;
        }
        let code = match action {
            Action::Create | Action::Update | Action::Replace | Action::Remove => "32",
            Action::Adopt | Action::Metadata | Action::Forget | Action::Orphaned => "36",
            Action::Conflict | Action::Skip => "33",
            Action::Fail => "31",
            Action::Unchanged => "2",
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

struct Counts {
    changed: usize,
    unchanged: usize,
    adopted: usize,
    conflicts: usize,
    errors: usize,
}

fn count(items: &[ItemReport]) -> Counts {
    let n = |f: &dyn Fn(Action) -> bool| items.iter().filter(|i| f(i.action)).count();
    Counts {
        changed: n(&|a| a.changes() || a == Action::Forget),
        unchanged: n(&|a| a == Action::Unchanged),
        adopted: n(&|a| a == Action::Adopt),
        conflicts: n(&|a| matches!(a, Action::Conflict | Action::Skip)),
        errors: n(&|a| a == Action::Fail),
    }
}

fn summary_counts(items: &[ItemReport]) -> serde_json::Value {
    let c = count(items);
    json!({
        "changed": c.changed,
        "unchanged": c.unchanged,
        "adopted": c.adopted,
        "conflicts": c.conflicts,
        "errors": c.errors,
    })
}

fn summary_line(items: &[ItemReport], run: RunKind) -> String {
    let dry_run = run != RunKind::Applied;
    let c = count(items);
    let mut parts = vec![
        format!("{} {}", c.changed, if dry_run { "to change" } else { "changed" }),
        format!("{} unchanged", c.unchanged),
    ];
    if c.adopted > 0 {
        parts.push(format!("{} adopted", c.adopted));
    }
    if c.conflicts > 0 {
        parts.push(format!("{} conflicts", c.conflicts));
    }
    if c.errors > 0 {
        parts.push(format!("{} errors", c.errors));
    }
    let line = parts.join(", ");
    match run {
        RunKind::Applied => line,
        RunKind::DryRun => format!("dry run, nothing written: {line}"),
        RunKind::Blocked => format!("{} found, nothing written: {line}", problems(&c)),
    }
}

/// What stopped a blocked run.
fn problems(c: &Counts) -> &'static str {
    match (c.conflicts > 0, c.errors > 0) {
        (true, true) => "conflicts and errors",
        (false, true) => "errors",
        _ => "conflicts",
    }
}

/// One file or registration a command looked at.
#[derive(Debug, Clone)]
pub struct Change {
    /// Repository-relative name.
    pub path: String,
    /// "ok" when nothing changed; otherwise the verb.
    pub action: &'static str,
    pub detail: Option<String>,
}

impl Change {
    pub fn new(path: &str, action: &'static str, detail: Option<String>) -> Change {
        Change {
            path: path.to_owned(),
            action,
            detail,
        }
    }
}

impl Printer {
    /// Lists sources with destination and mode (`template list`).
    pub fn sources(&self, ctx: &crate::repo::Context, sources: &[&crate::source::Source], key: &str) {
        let rows: Vec<(String, String, String)> = sources
            .iter()
            .map(|s| (s.path.to_string(), self.path(&ctx.dest_path(s)), s.mode.shown_label()))
            .collect();
        if self.json {
            let list: Vec<_> = rows
                .iter()
                .map(|(s, d, m)| json!({ "source": s, "dest": d, "mode": m }))
                .collect();
            outln!("{}", json!({ key: list }));
        } else {
            for (source, dest, mode) in &rows {
                outln!("{source} -> {dest} ({mode})");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(color: bool) -> Printer {
        Printer {
            json: false,
            quiet: false,
            verbose: false,
            color,
            home: None,
            warnings: Vec::new(),
        }
    }

    /// On a terminal each verb gets its color: changes green, notes cyan,
    /// conflicts yellow, errors red, unchanged dim. Without color, plain text.
    #[test]
    fn verbs_are_colored_by_kind_on_a_terminal() {
        let p = printer(true);
        let cases = [
            (Action::Create, "\x1b[32m  create\x1b[0m"),
            (Action::Remove, "\x1b[32m  remove\x1b[0m"),
            (Action::Adopt, "\x1b[36m   adopt\x1b[0m"),
            (Action::Orphaned, "\x1b[36morphaned\x1b[0m"),
            (Action::Conflict, "\x1b[33mconflict\x1b[0m"),
            (Action::Skip, "\x1b[33m    skip\x1b[0m"),
            (Action::Fail, "\x1b[31m   error\x1b[0m"),
            (Action::Unchanged, "\x1b[2m      ok\x1b[0m"),
        ];
        for (action, shown) in cases {
            assert_eq!(p.verb(action), shown, "{action:?}");
        }
        assert_eq!(printer(false).verb(Action::Create), "  create");
    }
}
