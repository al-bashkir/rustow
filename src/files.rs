//! `rustow file add|remove|list`: copy registrations. `file add` writes
//! the state of this machine; `[[files]]` in `rustow.toml` is the hand-written,
//! shared list. Registration never deploys or changes a destination.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use crate::config::{CONFIG_FILE, edit};
use crate::error::{Error, Result};
use crate::fsops;
use crate::paths::RelPath;
use crate::repo::Context;
use crate::report::{Change, Printer};
use crate::source::{Discovery, classify, discover};
use crate::state::State;

pub fn add(ctx: &mut Context, printer: &Printer, paths: &[String], dry_run: bool) -> Result<i32> {
    let mut state = ctx.load_state()?;
    let discovery = discover_registered(ctx, paths)?;
    let mut changes = Vec::new();
    for arg in paths {
        changes.push(register(ctx, &discovery, &mut state, arg)?);
    }
    finish(ctx, printer, &state, &changes, dry_run)
}

/// Saves the state when its registrations changed, unless `dry_run`, then reports.
fn finish(ctx: &Context, printer: &Printer, state: &State, changes: &[Change], dry_run: bool) -> Result<i32> {
    if !dry_run && state.files != ctx.config.added_files {
        state.save(&ctx.state_dir, &ctx.repo_id)?;
    }
    printer.changes(None, changes, dry_run);
    Ok(0)
}

/// The normal discovery, as if each argument that is a regular file
/// (with no symlink on its path) were registered. Such an argument is
/// then a copy source, or a skipped file with the reason it cannot be one.
fn discover_registered(ctx: &Context, paths: &[String]) -> Result<Discovery> {
    let mut config = ctx.config.clone();
    for arg in paths {
        let rel = RelPath::parse(arg)?;
        let abs = ctx.root.join(rel.as_path());
        if abs.is_file() && abs.canonicalize().is_ok_and(|real| real == abs) {
            config.files.entry(rel).or_default();
        }
    }
    discover(&ctx.root, &ctx.ignore, &config, &crate::template::Host::current())
}

/// Registers one source in the state. `discovery` comes from
/// `discover_registered`, so a selected file is a copy source.
fn register(ctx: &Context, discovery: &Discovery, state: &mut State, arg: &str) -> Result<Change> {
    let rel = RelPath::parse(arg)?;
    let selected = ctx.select(discovery, std::slice::from_ref(&arg.to_owned()))?;
    if !selected.iter().any(|s| s.path == rel) {
        return Err(Error::Usage(format!(
            "'{rel}' is a directory; `rustow file add` takes files"
        )));
    }
    if ctx.config.files.contains_key(&rel) || !state.files.insert(rel.clone()) {
        return Ok(Change::new(rel.as_str(), "ok", Some("already registered".into())));
    }
    Ok(Change::new(rel.as_str(), "register", None))
}

/// Removes registrations from the state and from `[[files]]` in `rustow.toml`.
pub fn remove(ctx: &mut Context, printer: &Printer, paths: &[String], dry_run: bool) -> Result<i32> {
    let mut state = ctx.load_state()?;
    let (old, mode) = read_config(ctx)?;
    let mut text = old.clone();
    let mut changes = Vec::new();
    for arg in paths {
        changes.push(unregister(&mut text, &mut state, arg)?);
    }
    if !dry_run && text != old {
        write_config(ctx, &text, mode)?;
    }
    finish(ctx, printer, &state, &changes, dry_run)
}

/// Removes one source's registration from the state and the configuration text.
fn unregister(text: &mut String, state: &mut State, arg: &str) -> Result<Change> {
    let rel = RelPath::parse(arg)?;
    let in_state = state.files.remove(&rel);
    // "" is a missing `rustow.toml` (an empty one fails to load before this).
    let edited = if text.is_empty() {
        None
    } else {
        edit::remove_file(text, &rel)?
    };
    let in_config = edited.is_some();
    if let Some(updated) = edited {
        *text = updated;
    }
    if !in_state && !in_config {
        return Ok(Change::new(rel.as_str(), "ok", Some("not registered".into())));
    }
    Ok(Change::new(
        rel.as_str(),
        "unregister",
        Some("deployed copy left in place".into()),
    ))
}

pub fn list(ctx: &mut Context, printer: &Printer) -> Result<i32> {
    ctx.load_state()?;
    let paths: std::collections::BTreeSet<&RelPath> = ctx.config.files.keys().chain(&ctx.config.added_files).collect();
    let rows: Vec<(String, String, String)> = paths
        .into_iter()
        .map(|path| match classify(path, &ctx.config) {
            Ok(s) => (path.to_string(), printer.path(&ctx.dest_path(&s)), s.mode.shown_label()),
            Err(e) => (path.to_string(), String::new(), format!("invalid: {e}")),
        })
        .collect();
    if printer.json {
        let files: Vec<_> = rows
            .iter()
            .map(|(s, d, m)| json!({ "source": s, "dest": d, "mode": m }))
            .collect();
        outln!("{}", json!({ "files": files }));
    } else {
        for (source, dest, mode) in &rows {
            outln!("{source} -> {dest} ({mode})");
        }
    }
    Ok(0)
}

/// Current configuration text ("" when absent) and its file mode.
fn read_config(ctx: &Context) -> Result<(String, u32)> {
    let path = ctx.root.join(CONFIG_FILE);
    match fs::symlink_metadata(&path) {
        Err(_) => Ok((String::new(), 0o644)),
        Ok(m) => {
            let text = String::from_utf8(fsops::read_file(&path)?)
                .map_err(|_| Error::Config(format!("{CONFIG_FILE} is not valid UTF-8")))?;
            Ok((text, m.permissions().mode() & 0o7777))
        }
    }
}

fn write_config(ctx: &Context, text: &str, mode: u32) -> Result<()> {
    fsops::write_atomic(&ctx.root.join(CONFIG_FILE), text.as_bytes(), mode, None, None)
}
