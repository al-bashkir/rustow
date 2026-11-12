//! `rustow file add|remove|list`: copy registrations in `.rustow.toml`.
//! Registration never deploys or changes a destination.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use crate::config::{CONFIG_FILE, edit};
use crate::error::{Error, Result};
use crate::fsops;
use crate::paths::RelPath;
use crate::repo::Context;
use crate::report::{Change, Printer};
use crate::source::{Discovery, Mode, classify, classify_unscoped, discover};

pub fn add(ctx: &Context, printer: &Printer, paths: &[String], dry_run: bool) -> Result<i32> {
    // Unscoped: an unregistered system file is a deploy error, but here it is what gets registered.
    let discovery = discover(&ctx.root, &ctx.ignore, &ctx.config, classify_unscoped)?;
    let (mut text, mode) = read_config(ctx)?;
    let mut changes = Vec::new();
    for arg in paths {
        changes.push(register(ctx, &discovery, &mut text, arg)?);
    }
    finish(ctx, printer, &text, mode, &changes, dry_run)
}

/// Registers one source in the configuration text.
fn register(ctx: &Context, discovery: &Discovery, text: &mut String, arg: &str) -> Result<Change> {
    let rel = RelPath::parse(arg)?;
    let selected = ctx.select(discovery, std::slice::from_ref(&arg.to_owned()))?;
    let Some(source) = selected.iter().find(|s| s.path == rel) else {
        return Err(Error::Usage(format!(
            "'{rel}' is a directory; `rustow file add` takes files"
        )));
    };
    let registered = || Change::new(rel.as_str(), "ok", Some("already registered".into()));
    match source.mode {
        Mode::Link | Mode::DecryptLink(_) => match edit::add_file(text, &rel)? {
            Some(updated) => {
                *text = updated;
                Ok(Change::new(rel.as_str(), "register", None))
            }
            None => Ok(registered()),
        },
        Mode::FileCopy | Mode::DecryptCopy(_) => Ok(registered()),
        Mode::TemplateLink(_) | Mode::TemplateCopy(..) => Err(Error::Usage(format!(
            "'{rel}' is a template; templates are copied by giving them [[template_rules]] metadata"
        ))),
    }
}

/// Writes the configuration when something changed, then reports.
fn finish(ctx: &Context, printer: &Printer, text: &str, mode: u32, changes: &[Change], dry_run: bool) -> Result<i32> {
    if !dry_run && changes.iter().any(|c| c.action != "ok") {
        write_config(ctx, text, mode)?;
    }
    printer.changes(None, changes, dry_run);
    Ok(0)
}

pub fn remove(ctx: &Context, printer: &Printer, paths: &[String], dry_run: bool) -> Result<i32> {
    let (mut text, mode) = read_config(ctx)?;
    let mut changes = Vec::new();
    for arg in paths {
        changes.push(unregister(&mut text, arg)?);
    }
    finish(ctx, printer, &text, mode, &changes, dry_run)
}

/// Removes one source's registration from the configuration text.
fn unregister(text: &mut String, arg: &str) -> Result<Change> {
    let rel = RelPath::parse(arg)?;
    match edit::remove_file(text, &rel)? {
        Some(updated) => {
            *text = updated;
            Ok(Change::new(
                rel.as_str(),
                "unregister",
                Some("deployed copy left in place".into()),
            ))
        }
        None => Ok(Change::new(rel.as_str(), "ok", Some("not registered".into()))),
    }
}

pub fn list(ctx: &Context, printer: &Printer) -> Result<i32> {
    let rows: Vec<(String, String, String)> = ctx
        .config
        .files
        .iter()
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
        println!("{}", json!({ "files": files }));
    } else {
        for (source, dest, mode) in &rows {
            println!("{source} -> {dest} ({mode})");
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
