//! `rustow diff`: desired against deployed content, from the same plan
//! that `apply` executes. The output may contain secrets by design.

use std::path::Path;

use crate::diff;
use crate::error::Result;
use crate::fsops::{self, FileMeta, Observed};
use crate::plan::{Deploy, Desired, Plan, Step};
use crate::report::Printer;

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
