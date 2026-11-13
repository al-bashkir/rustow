//! `rustow doctor`: a read-only report on the environment.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::unistd::{AccessFlags, access, geteuid};
use serde_json::json;

use crate::crypto;
use crate::error::Result;
use crate::fsops::TEMP_PREFIX;
use crate::repo::Context;
use crate::report::Printer;

struct Check {
    level: &'static str,
    name: &'static str,
    detail: String,
    /// The detail is ready to print: its names went through `shown`, and
    /// a shell command in it must not (the lines of `pending::report`).
    shown: bool,
}

fn check(level: &'static str, name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level,
        name,
        detail: detail.into(),
        shown: false,
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
    checks.extend(backups(&ctx.state_dir, uid));
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
            let detail = match c.shown {
                true => c.detail.as_str().into(),
                false => crate::report::shown(&c.detail),
            };
            outln!("{:>5}  {}: {detail}", c.level, c.name);
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

/// The temporary files that an interrupted run left: the ones the journal
/// lists, and the others in the repository.
fn temp_files(ctx: &Context) -> Vec<Check> {
    let journal = ctx.state_dir.join(format!("{}.pending", ctx.repo_id));
    let mut out: Vec<Check> = match crate::pending::report(&journal) {
        Ok(left) => (left.into_iter())
            .map(|line| Check {
                shown: true,
                ..check("warn", "temp file", line)
            })
            .collect(),
        Err(e) => vec![check("warn", "temp files", e.to_string())],
    };
    let journaled = crate::pending::entries(&journal).unwrap_or_default();
    out.extend(unjournaled(&ctx.root, &journaled));
    if out.is_empty() {
        out.push(check("ok", "temp files", "none left by an interrupted run"));
    }
    out
}

/// Each file in the repository (not in `.git`) with a temporary name that
/// the journal does not list. `enc` sets the plaintext aside under such a
/// name and does not journal it, because it can be the only copy.
fn unjournaled(root: &Path, journaled: &[PathBuf]) -> Vec<Check> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git")
        .flatten()
        .filter(|e| e.file_type().is_file() && e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
        .filter(|e| !journaled.iter().any(|j| j == e.path()))
        .map(|e| {
            let detail = format!(
                "{}: an interrupted run left it, and it can hold the plaintext of a file that \
                 `rustow enc` encrypted; rustow never removes it. Look at it, then rename it back \
                 to its old name or remove it",
                e.path().display()
            );
            check("warn", "temp file", detail)
        })
        .collect()
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

/// A warning for each directory under `<state_dir>/backups` that the user
/// does not own: a backup into it fails. Only the topmost one counts,
/// because the advice fixes everything below it.
fn backups(state_dir: &Path, uid: u32) -> Vec<Check> {
    let dir = state_dir.join("backups");
    let mut foreign = Vec::new();
    foreign_dirs(&dir, uid, &mut foreign);
    if foreign.is_empty() {
        return vec![check(
            "ok",
            "backups",
            format!("{} has only your directories", dir.display()),
        )];
    }
    foreign
        .into_iter()
        .map(|(path, owner)| {
            let path = path.display();
            let advice = format!("give it back with `sudo chown -R {uid} -- {path}`");
            check(
                "warn",
                "backups",
                format!("{path} is owned by uid {owner}, not uid {uid}, so backups there fail; {advice}"),
            )
        })
        .collect()
}

/// Pushes `dir` when it is a directory of another owner, else does the
/// same for each entry of it. Symlinks are not followed.
fn foreign_dirs(dir: &Path, uid: u32, out: &mut Vec<(PathBuf, u32)>) {
    let Ok(meta) = fs::symlink_metadata(dir) else { return };
    if !meta.is_dir() {
        return;
    }
    if meta.uid() != uid {
        return out.push((dir.to_path_buf(), meta.uid()));
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        foreign_dirs(&entry.path(), uid, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unjournaled_temp_files_skip_git_directories_and_the_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in [
            "a/.rustow-tmp-aside",
            ".rustow-tmp-journaled",
            ".git/.rustow-tmp-git",
            "a/plain",
            ".rustow-tmp-dir/x",
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "x").unwrap();
        }
        let found = unjournaled(root, &[root.join(".rustow-tmp-journaled")]);
        assert_eq!(
            found.len(),
            1,
            "{:?}",
            found.iter().map(|c| &c.detail).collect::<Vec<_>>()
        );
        let aside = root.join("a/.rustow-tmp-aside");
        assert!(
            found[0]
                .detail
                .starts_with(&format!("{}: an interrupted run left it", aside.display())),
            "{}",
            found[0].detail
        );
        assert_eq!((found[0].level, found[0].name), ("warn", "temp file"));
    }

    #[test]
    fn a_backup_directory_of_another_owner_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("backups/id/time")).unwrap();
        let me = geteuid().as_raw();
        let mine = backups(tmp.path(), me);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].level, "ok", "{}", mine[0].detail);
        let other = backups(tmp.path(), me + 1);
        assert_eq!(other.len(), 1, "only the topmost directory");
        assert_eq!(other[0].level, "warn");
        let top = tmp.path().join("backups");
        assert_eq!(
            other[0].detail,
            format!(
                "{0} is owned by uid {me}, not uid {1}, so backups there fail; give it back with `sudo chown -R {1} -- {0}`",
                top.display(),
                me + 1
            )
        );
    }
}
