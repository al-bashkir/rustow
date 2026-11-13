//! Spec §17.2 Scenario H — conflict detection (symlink part).

mod common;

use common::*;
use std::fs;

fn two() -> Fixture {
    let fx = Fixture::new();
    fx.write(".bashrc", "repo bash\n");
    fx.write(".zshrc", "repo zsh\n");
    fx
}

#[test]
fn default_skips_conflicts_deploys_the_rest_and_lists_conflicts_last() {
    let fx = two();
    fx.write_home(".bashrc", "mine\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "mine\n");
    assert!(is_symlink(&fx.home_path(".zshrc")));
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "  create ~/.zshrc  <- .zshrc (link)",
            "Conflicts:",
            "    skip ~/.bashrc  <- .bashrc (link): existing destination is not managed by rustow",
            "1 changed, 0 unchanged, 1 conflicts",
        ],
        "{out:?}"
    );
    let quiet = fx.run(&["apply", "-q"]);
    assert_eq!(
        quiet.stdout,
        "Conflicts:\n    skip ~/.bashrc  <- .bashrc (link): existing destination is not managed by rustow\n"
    );
    let clean = Fixture::new();
    clean.write(".bashrc", "b\n");
    assert!(!clean.ok(&["apply"]).stdout.contains("Conflicts:"));
}

#[test]
fn abort_policy_conflict_exit_3_nothing_changed() {
    let fx = two();
    fx.write_home(".bashrc", "mine\n");
    let out = fx.run(&["apply", "--on-conflict", "abort"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        out.stdout.contains("conflict") && out.stdout.contains(".bashrc"),
        "{out:?}"
    );
    assert!(out.stdout.contains("not managed by rustow"), "{out:?}");
    assert!(out.stdout.contains("conflicts found, nothing written"), "{out:?}");
    assert!(!out.stdout.contains("dry run"), "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "mine\n");
    assert!(!fx.home_path(".zshrc").exists(), "abort policy changes nothing");
}

#[test]
fn unmanaged_symlink_conflict() {
    let fx = two();
    std::os::unix::fs::symlink("/somewhere/else", fx.home_path(".bashrc")).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(link_text(&fx.home_path(".bashrc")).to_str(), Some("/somewhere/else"));
}

#[test]
fn foreign_broken_symlink_conflict_then_force() {
    let fx = two();
    std::os::unix::fs::symlink("dotfiles/gone", fx.home_path(".bashrc")).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("broken symlink"), "{out:?}");
    fx.ok(&["apply", "--force"]);
    assert_eq!(read(&fx.home_path(".bashrc")), "repo bash\n");
}

#[test]
fn adopts_existing_correct_symlink() {
    let fx = two();
    std::os::unix::fs::symlink("dotfiles/.bashrc", fx.home_path(".bashrc")).unwrap();
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("adopt"), "{out:?}");
    // Adopted links are managed: unlink removes them.
    fx.ok(&["unlink", ".bashrc"]);
    assert!(!is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn backup_moves_conflicting_file() {
    let fx = two();
    fx.write_home(".bashrc", "mine\n");
    let out = fx.ok(&["apply", "--backup"]);
    assert!(out.stdout.contains("backup"), "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "repo bash\n");
    let backups: Vec<_> = walk(&fx.state.join("rustow/backups"));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(read(&backups[0]), "mine\n");
    assert!(backups[0].ends_with("home/.bashrc"), "{backups:?}");
}

#[test]
fn force_never_replaces_dir() {
    let fx = two();
    fs::create_dir_all(fx.home_path(".bashrc/keep")).unwrap();
    let out = fx.run(&["apply", "--force"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(fx.home_path(".bashrc/keep").is_dir());
}

#[test]
fn unlink_removes_only_ours() {
    let fx = two();
    fx.ok(&["apply"]);
    fs::remove_file(fx.home_path(".zshrc")).unwrap();
    fx.write_home(".zshrc", "user replaced it\n");
    let out = fx.run(&["unlink"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!fx.home_path(".bashrc").exists());
    assert_eq!(read(&fx.home_path(".zshrc")), "user replaced it\n");
    assert!(fx.repo.join(".bashrc").exists(), "sources are never removed");
}

#[test]
fn unlink_removes_created_empty_dirs_only() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\nfold_directories = false\n");
    fx.write(".config/nvim/init.lua", "x");
    fx.write(".config/git/ignore", "y");
    fx.ok(&["apply"]);
    fx.write_home(".config/git/user-file", "keep");
    fx.ok(&["unlink"]);
    assert!(!fx.home_path(".config/nvim").exists(), "created and now empty");
    assert!(fx.home_path(".config/git/user-file").exists());
    assert!(fx.home_path(".config").is_dir());
}

#[test]
fn unlink_dry_run_changes_nothing() {
    let fx = two();
    fx.ok(&["apply"]);
    let out = fx.ok(&["unlink", "--dry-run"]);
    assert!(out.stdout.contains("remove"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn unlink_handles_removed_source() {
    let fx = two();
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join(".zshrc")).unwrap();
    fx.ok(&["unlink", ".zshrc"]);
    assert!(!is_symlink(&fx.home_path(".zshrc")));
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn missing_parent_without_create_directories_conflicts() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\ncreate_directories = false\n");
    fx.write(".config/x", "x");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!fx.home_path(".config").exists());
}

#[test]
fn second_run_is_locked_out_while_first_holds_lock() {
    let fx = two();
    fs::create_dir_all(fx.state.join("rustow")).unwrap();
    // Hold the lock of whatever repo id rustow computes by locking every *.lock it creates.
    fx.ok(&["apply"]);
    let lock = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "lock"))
        .expect("lock file");
    let held = fs::OpenOptions::new().write(true).open(&lock).unwrap();
    held.try_lock().unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("another rustow"), "{out:?}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Spec §14.6: a home link is trust-checked like a system link. With a
/// root-owned target (as under `sudo -H`), a link into the user's
/// repository is refused. A dry run never writes, so /usr is safe here.
#[test]
fn home_link_into_a_repository_the_target_owner_does_not_own_is_untrusted() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root owns the repository too");
        return;
    }
    let fx = Fixture::new();
    fx.write(".bashrc", "x");
    fx.write(".t.tmpl", "y");
    let out = fx.run(&["--target", "/usr", "apply", "--dry-run"]);
    assert_eq!(out.code, 3, "{out:?}");
    let line = |src: &str| {
        out.stdout
            .lines()
            .find(|l| l.contains(src))
            .unwrap_or_else(|| panic!("no line for {src}: {out:?}"))
            .to_owned()
    };
    let plain = line("<- .bashrc");
    assert!(plain.contains("skip") && plain.contains("untrusted"), "{plain}");
    assert!(
        plain.contains("repository source") && plain.contains("rustow file add"),
        "{plain}"
    );
    assert!(!plain.contains("cache") && !plain.contains("template_rules"), "{plain}");
    let cached = line("<- .t.tmpl");
    assert!(
        cached.contains("untrusted") && cached.contains("cache entry"),
        "{cached}"
    );
}

/// Review I5: a home symlink into a source that others can modify is
/// refused, as for cache-backed links. (A group-writable directory may be
/// trusted in a user-private group; see the next test.)
#[test]
fn home_link_into_writable_source_tree_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write(".config/app/x.conf", "x");
    let dir = fx.repo.join(".config/app");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o757)).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("writable by others"), "{out:?}");
    assert!(!fx.home_path(".config/app/x.conf").exists());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    fx.ok(&["apply"]);
    assert!(
        is_symlink(&fx.home_path(".config/app")),
        "folded once the directory is safe"
    );
}

/// With umask 002, a repository is group-writable. That is trusted only
/// when the group is the user's private group (named like the user, no
/// other members); a shared primary group, as on some CI runners, is not.
#[test]
fn group_writable_repository_is_trusted_only_in_a_user_private_group() {
    use nix::unistd::{Group, User, geteuid};
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let src = fx.write(".config/app/x", "x\n");
    let user = User::from_uid(geteuid()).unwrap().expect("test user");
    let group = Group::from_gid(user.gid).unwrap().expect("primary group");
    let private = group.name == user.name && group.mem.iter().all(|m| *m == user.name);
    for p in src.ancestors().take_while(|p| p.starts_with(&fx.base)) {
        std::os::unix::fs::chown(p, None, Some(user.gid.as_raw())).unwrap();
        let mode = fs::metadata(p).unwrap().permissions().mode();
        fs::set_permissions(p, fs::Permissions::from_mode(mode | 0o020)).unwrap();
    }
    let out = fx.run(&["apply"]);
    if private {
        assert_eq!(out.code, 0, "{out:?}");
        assert_eq!(read(&fx.home_path(".config/app/x")), "x\n");
    } else {
        assert_eq!(out.code, 3, "{out:?}");
        assert!(
            out.stdout.contains("untrusted") && out.stdout.contains("chmod g-w"),
            "{out:?}"
        );
        assert!(!fx.home_path(".config/app/x").exists());
    }
}

/// A dry run predicts what the real run cannot write: a destination below
/// a directory the user cannot write fails in the plan, with the directory
/// named and the privilege hint, and the real run tries nothing there.
#[test]
fn unwritable_target_directory_fails_in_the_plan_and_the_dry_run() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root can write everywhere");
        return;
    }
    let fx = Fixture::new();
    fx.write(".ro/x", "x");
    fx.write(".ro/new/y", "y");
    fx.write(".ok", "ok");
    let ro = fx.home_path(".ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let out = fx.run(&["apply", "--dry-run"]);
    assert_eq!(out.code, 4, "{out:?}");
    let named = format!("directory {}:", ro.display());
    // `.ro` is a direct child of home and never folds; `.ro/new` folds.
    for src in ["<- .ro/x", "<- .ro/new (link)"] {
        let line = out.stdout.lines().find(|l| l.contains(src)).unwrap_or_default();
        assert!(line.contains("error") && line.contains(&named), "{out:?}");
        assert!(line.contains("privileges"), "{out:?}");
    }
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(!out.stdout.contains("changed while"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".ok")));
    assert_eq!(fs::read_dir(&ro).unwrap().count(), 0, "nothing tried in ~/.ro");
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `apply` names a managed destination whose source was removed, with
/// the command that removes it, and removes nothing itself. The exit code
/// stays 0; the JSON output carries the same item.
#[test]
fn apply_reports_orphaned_destinations_and_keeps_them() {
    let fx = two();
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join(".zshrc")).unwrap();
    let out = fx.ok(&["apply"]);
    let line = out.stdout.lines().find(|l| l.contains("~/.zshrc")).unwrap_or_default();
    assert!(line.contains("orphaned"), "{out:?}");
    assert!(
        line.contains("source removed; `rustow undeploy .zshrc` removes it"),
        "{out:?}"
    );
    assert!(is_symlink(&fx.home_path(".zshrc")));
    let out = fx.ok(&["apply", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    let items = v["items"].as_array().unwrap();
    let item = items.iter().find(|i| i["source"] == ".zshrc").expect("orphan item");
    assert_eq!(item["action"], "orphaned");
    assert!(item["detail"].as_str().unwrap().contains("`rustow undeploy .zshrc`"));
    fx.ok(&["undeploy", ".zshrc"]);
    assert!(!is_symlink(&fx.home_path(".zshrc")));
    assert!(
        is_symlink(&fx.home_path(".bashrc")),
        "the suggestion removes only the orphan"
    );
}

/// Only a foreign symlink that points nowhere is called broken.
#[test]
fn only_a_dangling_foreign_symlink_is_called_broken() {
    let fx = two();
    fx.write_home("other", "x\n");
    std::os::unix::fs::symlink("other", fx.home_path(".bashrc")).unwrap();
    std::os::unix::fs::symlink("gone", fx.home_path(".zshrc")).unwrap();
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 3, "{out:?}");
    let line = |dest| out.stdout.lines().find(|l| l.contains(dest)).unwrap_or_default();
    assert!(line("~/.zshrc").contains("broken symlink to gone"), "{out:?}");
    assert!(line("~/.bashrc").contains("not managed by rustow"), "{out:?}");
    assert!(!line("~/.bashrc").contains("broken"), "{out:?}");
}

/// Only a decision that writes needs a writable directory: a link that is
/// already deployed stays clean after its directory becomes read-only.
#[test]
fn deployed_link_in_a_read_only_directory_stays_clean() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root can write everywhere");
        return;
    }
    let fx = Fixture::new();
    fx.write(".ro/x", "x");
    fx.ok(&["apply"]);
    let ro = fx.home_path(".ro");
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let out = fx.run(&["apply"]);
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains("0 changed"), "{out:?}");
    assert_eq!(read(&fx.home_path(".ro/x")), "x");
}

/// Orphans follow the path arguments. A file source replaced by a
/// directory is still an orphan: only a fold record is known by its directory.
#[test]
fn orphans_follow_path_arguments_and_a_source_turned_directory() {
    let fx = two();
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join(".zshrc")).unwrap();
    let out = fx.ok(&["apply", ".bashrc"]);
    assert!(!out.stdout.contains(".zshrc"), "not selected: {out:?}");
    fs::create_dir(fx.repo.join(".zshrc")).unwrap();
    let out = fx.ok(&["apply"]);
    let line = out.stdout.lines().find(|l| l.contains("~/.zshrc")).unwrap_or_default();
    assert!(line.contains("orphaned"), "{out:?}");
}

/// Names that differ only in case are two files on a case-sensitive
/// target and one file on a case-insensitive one (APFS by default). Linux
/// covers only the first branch; macOS CI covers the second.
#[test]
fn names_that_differ_only_in_case_collide_only_on_a_case_insensitive_target() {
    let fx = Fixture::new();
    fx.write("X", "upper\n");
    fx.write("x.tmpl", "lower\n");
    fs::write(fx.base.join("probe"), "").unwrap();
    let insensitive = fx.base.join("PROBE").exists();
    let out = fx.run(&["apply"]);
    if insensitive {
        assert_eq!(out.code, 3, "{out:?}");
        assert!(out.stdout.contains("same destination"), "{out:?}");
        assert!(!fx.home_path("x").exists());
    } else {
        assert_eq!(out.code, 0, "{out:?}");
        assert_eq!(read(&fx.home_path("X")), "upper\n");
        assert_eq!(read(&fx.home_path("x")), "lower\n");
    }
}

/// A link rustow recorded and the user then retargeted or replaced is
/// MODIFIED, not unmanaged; it is still never replaced without a policy.
#[test]
fn changed_managed_link_is_modified() {
    let fx = two();
    fx.ok(&["apply"]);
    fx.write_home("other", "x\n");
    fs::remove_file(fx.home_path(".bashrc")).unwrap();
    std::os::unix::fs::symlink("other", fx.home_path(".bashrc")).unwrap();
    fs::remove_file(fx.home_path(".zshrc")).unwrap();
    fx.write_home(".zshrc", "mine\n");

    let status = fx.run(&["status"]);
    let line = |out: &Out, dest: &str| {
        out.stdout
            .lines()
            .find(|l| l.contains(dest))
            .unwrap_or_default()
            .to_string()
    };
    assert!(line(&status, "~/.bashrc").contains("MODIFIED"), "{status:?}");
    assert!(line(&status, "~/.zshrc").contains("MODIFIED"), "{status:?}");

    let apply = fx.run(&["apply"]);
    assert_eq!(apply.code, 3, "{apply:?}");
    assert!(
        line(&apply, "~/.bashrc").contains("modified since rustow last applied"),
        "{apply:?}"
    );
    assert_eq!(link_text(&fx.home_path(".bashrc")), std::path::PathBuf::from("other"));
    assert_eq!(read(&fx.home_path(".zshrc")), "mine\n");
}

/// A target reached through a symlink gets the check of a symlinked
/// ancestor: a target directory that others can write stops the command,
/// so a copy never lands where another user can read or replace it.
#[test]
fn a_target_behind_a_symlink_to_a_world_writable_directory_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \".cfg\"\n");
    fx.write(".cfg", "token\n");
    let open = fx.base.join("open");
    fs::create_dir(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    let target = fx.base.join("tt");
    std::os::unix::fs::symlink(&open, &target).unwrap();
    let out = fx.run(&["--target", target.to_str().unwrap(), "apply"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains("target") && out.stderr.contains("writable by others"),
        "{out:?}"
    );
    assert!(!open.join(".cfg").exists());
}
