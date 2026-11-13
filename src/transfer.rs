//! `rustow state export|import`: the registrations of `file add` and
//! copy-mode `link` as JSON, to move them to another machine. The records
//! of deployed files stay: they belong to one machine.

use std::collections::BTreeSet;
use std::path::Path;

use nix::fcntl::OFlag;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::fsops;
use crate::paths::RelPath;
use crate::repo::Context;
use crate::report::{Change, Printer};
use crate::state::{MAX_STATE_SIZE, State};

/// Version of the export format; independent of the state version.
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registrations {
    version: u32,
    #[serde(default)]
    files: BTreeSet<RelPath>,
    #[serde(default)]
    links: BTreeSet<RelPath>,
}

/// Prints the registrations as JSON, or writes them to `file`.
pub fn export(ctx: &Context, file: Option<&Path>) -> Result<i32> {
    let state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let regs = Registrations {
        version: FORMAT,
        files: state.files,
        links: state.links,
    };
    let mut text =
        serde_json::to_string_pretty(&regs).map_err(|e| Error::Failed(format!("encode registrations: {e}")))?;
    text.push('\n');
    match file {
        Some(path) => fsops::write_atomic(path, text.as_bytes(), 0o644, None, None)?,
        None => crate::report::out(format_args!("{text}")),
    }
    Ok(0)
}

/// Adds the registrations in `file` ("-" is stdin) to the state of this
/// machine. Registrations that are there already stay. Each path must pass
/// the check of `file add` or of `link <path>`; else nothing changes.
pub fn import(ctx: &mut Context, printer: &Printer, file: &Path, dry_run: bool) -> Result<i32> {
    let regs = read(file)?;
    let mut state = ctx.load_state()?;
    check_paths(ctx, &state, &regs)?;
    let changes = add(&mut state, regs);
    if !dry_run && !changes.is_empty() {
        state.save(&ctx.state_dir, &ctx.repo_id)?;
    }
    printer.changes(None, &changes, dry_run);
    Ok(0)
}

/// Adds each registration that `state` does not hold yet; one change for each.
fn add(state: &mut State, regs: Registrations) -> Vec<Change> {
    let mut changes = Vec::new();
    for (list, into, kind) in [
        (regs.files, &mut state.files, "copy"),
        (regs.links, &mut state.links, "link"),
    ] {
        for path in list {
            if into.insert(path.clone()) {
                changes.push(Change::new(path.as_str(), "import", Some(kind.into())));
            }
        }
    }
    changes
}

/// Checks each `files` path as `file add` does and each `links` path as
/// `link <path>` does.
fn check_paths(ctx: &Context, state: &State, regs: &Registrations) -> Result<()> {
    let files: Vec<String> = regs.files.iter().map(ToString::to_string).collect();
    crate::files::check_copies(ctx, &files)?;
    let discovery = ctx.discover()?;
    regs.links
        .iter()
        .try_for_each(|link| crate::app::check_link(ctx, &discovery, state, link.as_str()))
}

/// Reads and checks an export. Each path is checked as a repository path.
fn read(file: &Path) -> Result<Registrations> {
    let bad = |why: String| Error::Usage(format!("{}: {why}", file.display()));
    let bytes = match file == Path::new("-") {
        true => fsops::read_at_most(std::io::stdin(), "standard input", MAX_STATE_SIZE),
        false => fsops::read_limited(file, OFlag::empty(), MAX_STATE_SIZE),
    }
    .map_err(|e| Error::Usage(e.to_string()))?;
    let regs: Registrations = serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?;
    if regs.version != FORMAT {
        return Err(bad(format!(
            "unsupported registrations version {} (this rustow reads {FORMAT})",
            regs.version
        )));
    }
    Ok(regs)
}
