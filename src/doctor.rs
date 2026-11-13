//! `rustow doctor`: a read-only report on the environment.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use nix::unistd::{AccessFlags, access, geteuid};
use serde_json::json;

use crate::crypto;
use crate::error::Result;
use crate::repo::Context;
use crate::report::Printer;

struct Check {
    level: &'static str,
    name: &'static str,
    detail: String,
}

fn check(level: &'static str, name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level,
        name,
        detail: detail.into(),
    }
}

/// Prints one line per check; exit 1 when any check fails.
pub fn doctor(ctx: &Context, printer: &Printer) -> Result<i32> {
    let uid = geteuid().as_raw();
    let mut checks = vec![
        check("ok", "repository", ctx.root.display().to_string()),
        match ctx.config.present {
            true => check("ok", "config", "rustow.toml found"),
            false => check("warn", "config", "no rustow.toml; defaults apply (run `rustow init`)"),
        },
        private_dir("cache", &ctx.cache_dir, uid),
        private_dir("state", &ctx.state_dir, uid),
        writable("target", &ctx.home_target, "fail"),
        writable("root target", &ctx.root_target, "warn"),
        check(
            "info",
            "privileges",
            format!("running as uid {uid}{}", if uid == 0 { " (root)" } else { "" }),
        ),
    ];
    checks.extend(temp_files(ctx));
    checks.extend(encryption_checks(ctx));
    checks.push(match crate::fsops::find_program("gpg") {
        Some(path) => check("ok", "gpg", path.display().to_string()),
        None => check("warn", "gpg", "not found in PATH; needed only for .gpg sources"),
    });
    if printer.json {
        let list: Vec<_> = checks
            .iter()
            .map(|c| json!({ "level": c.level, "check": c.name, "detail": c.detail }))
            .collect();
        outln!("{}", json!({ "checks": list }));
    } else {
        for c in &checks {
            outln!("{:>5}  {}: {}", c.level, c.name, crate::report::shown(&c.detail));
        }
    }
    Ok(i32::from(checks.iter().any(|c| c.level == "fail")))
}

fn private_dir(name: &'static str, dir: &Path, uid: u32) -> Check {
    let shown = dir.display();
    match fs::symlink_metadata(dir) {
        Err(_) => check(
            "warn",
            name,
            format!("{shown} does not exist yet; created on first use"),
        ),
        Ok(m) if !m.is_dir() => check(
            "fail",
            name,
            format!("{shown} is not a directory (symlinks are not followed)"),
        ),
        Ok(m) if m.uid() != uid => check(
            "fail",
            name,
            format!("{shown} is owned by uid {}, not uid {uid}", m.uid()),
        ),
        Ok(m) if m.permissions().mode() & 0o7077 != 0 => check(
            "warn",
            name,
            format!(
                "{shown} has mode {:04o}; expected 0700 (rustow tightens it on use)",
                m.permissions().mode() & 0o7777
            ),
        ),
        Ok(m) => check("ok", name, format!("{shown} ({:04o})", m.permissions().mode() & 0o7777)),
    }
}

fn writable(name: &'static str, dir: &Path, level: &'static str) -> Check {
    match access(dir, AccessFlags::W_OK) {
        Ok(()) => check("ok", name, format!("{} is writable", dir.display())),
        Err(_) => check(
            level,
            name,
            format!(
                "{} is not writable by this user; deploying there needs privileges",
                dir.display()
            ),
        ),
    }
}

/// The temporary files that an interrupted run left (the journal lists them).
fn temp_files(ctx: &Context) -> Vec<Check> {
    let journal = ctx.state_dir.join(format!("{}.pending", ctx.repo_id));
    match crate::pending::report(&journal) {
        Ok(left) if left.is_empty() => vec![check("ok", "temp files", "none left by an interrupted run")],
        Ok(left) => left.into_iter().map(|line| check("warn", "temp file", line)).collect(),
        Err(e) => vec![check("warn", "temp files", e.to_string())],
    }
}

/// Age identity files and the GPG home, without loading any key.
fn encryption_checks(ctx: &Context) -> Vec<Check> {
    let forbidden = [ctx.root.as_path(), ctx.cache_dir.as_path()];
    let mut out = age_checks(ctx, &forbidden);
    out.extend(gpg_check(ctx, &forbidden));
    out
}

fn age_checks(ctx: &Context, forbidden: &[&Path]) -> Vec<Check> {
    match crypto::identity_paths(ctx) {
        Err(e) => vec![check("fail", "age identity", e)],
        Ok(files) if files.is_empty() => vec![check("info", "age identity", "none configured")],
        Ok(files) => files
            .iter()
            .map(|f| match crypto::age::check_identity_file(f, forbidden) {
                Ok(()) => check("ok", "age identity", f.display().to_string()),
                Err(e) => check("fail", "age identity", e),
            })
            .collect(),
    }
}

/// The GPG home check, only when GPG is configured.
fn gpg_check(ctx: &Context, forbidden: &[&Path]) -> Option<Check> {
    let gpg = &ctx.config.encryption.gpg;
    if gpg.recipients.is_empty() && gpg.homedir.is_none() {
        return None;
    }
    Some(
        match crypto::gpg_homedir(ctx).and_then(|h| crypto::gpg::check_homedir(&h, forbidden).map(|()| h)) {
            Ok(home) => check("ok", "gpg home", home.display().to_string()),
            Err(e) => check("fail", "gpg home", e),
        },
    )
}
