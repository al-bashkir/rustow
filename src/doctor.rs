//! `rustow doctor`: a read-only report on the environment.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::unistd::{AccessFlags, access, geteuid};
use serde_json::json;

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
            true => check("ok", "config", ".rustow.toml found"),
            false => check("warn", "config", "no .rustow.toml; defaults apply (run `rustow init`)"),
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
    checks.push(match find_in_path("gpg") {
        Some(path) => check("ok", "gpg", path.display().to_string()),
        None => check("warn", "gpg", "not found in PATH; needed only for .gpg sources"),
    });
    if printer.json {
        let list: Vec<_> = checks
            .iter()
            .map(|c| json!({ "level": c.level, "check": c.name, "detail": c.detail }))
            .collect();
        println!("{}", json!({ "checks": list }));
    } else {
        for c in &checks {
            println!("{:>5}  {}: {}", c.level, c.name, c.detail);
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
        Ok(m) if m.permissions().mode() & 0o077 != 0 => check(
            "warn",
            name,
            format!(
                "{shown} has mode {:04o}; expected 0700 (rustow tightens it on use)",
                m.permissions().mode() & 0o7777
            ),
        ),
        Ok(_) => check("ok", name, format!("{shown} (0700)")),
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

/// First executable file called `name` in `PATH`.
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}
