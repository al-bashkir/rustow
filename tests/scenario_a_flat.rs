//! Spec §17.2 Scenario A — flat repository — plus Review Focus 1–4.

mod common;

use common::*;
use std::fs;
use std::path::PathBuf;

/// Per-file links: folding is off, so each file gets its own symlink.
fn flat() -> Fixture {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\nfold_directories = false\n");
    fx.write(".bashrc", "# bash\n");
    fx.write(".config/nvim/init.lua", "-- nvim\n");
    fx
}

#[test]
fn flat_mapping() {
    let fx = flat();
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".bashrc")), "# bash\n");
    assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "-- nvim\n");
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
}

#[test]
fn symlink_targets_relative() {
    let fx = flat();
    fx.ok(&["apply"]);
    assert_eq!(link_text(&fx.home_path(".bashrc")), PathBuf::from("dotfiles/.bashrc"));
    assert_eq!(
        link_text(&fx.home_path(".config/nvim/init.lua")),
        PathBuf::from("../../dotfiles/.config/nvim/init.lua")
    );
}

#[test]
fn creates_parent_dirs() {
    let fx = flat();
    fx.ok(&["apply"]);
    let meta = fs::symlink_metadata(fx.home_path(".config/nvim")).unwrap();
    assert!(meta.is_dir(), "parent must be a real directory, not a folded link");
}

#[test]
fn second_apply_changes_nothing() {
    let fx = flat();
    fx.ok(&["apply"]);
    let before = mtime(&fx.home_path(".bashrc"));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("0 changed"), "{out:?}");
    assert_eq!(mtime(&fx.home_path(".bashrc")), before);
}

#[test]
fn bare_rustow_equals_apply() {
    let fx = flat();
    fx.ok(&[]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
}

#[test]
fn dry_run_changes_nothing_and_lists_ops() {
    let fx = flat();
    let out = fx.ok(&["apply", "--dry-run"]);
    assert!(
        out.stdout.contains(".bashrc") && out.stdout.contains("create"),
        "{out:?}"
    );
    assert!(out.stdout.contains("dry run"), "{out:?}");
    assert!(!fx.home_path(".bashrc").exists());
    assert!(!fx.home_path(".config").exists());
    assert!(!fx.state.exists(), "dry run must not write state");
}

#[test]
fn selected_paths_only() {
    let fx = flat();
    fx.ok(&["apply", ".config/nvim"]);
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
    assert!(!fx.home_path(".bashrc").exists());
}

#[test]
fn json_output_lists_items() {
    let fx = flat();
    let out = fx.ok(&["apply", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["source"], ".bashrc");
    assert_eq!(items[0]["action"], "create");
    assert_eq!(items[0]["mode"], "link");
}

// Review Focus 1
#[test]
fn link_works_when_target_root_is_symlink() {
    let fx = flat();
    let alias = fx.base.join("home-alias");
    std::os::unix::fs::symlink(&fx.home, &alias).unwrap();
    let out = fx.run_env(&["apply"], &[("HOME", alias.to_str().unwrap())]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "# bash\n");
    let again = fx.run_env(&["apply"], &[("HOME", alias.to_str().unwrap())]);
    assert!(again.stdout.contains("0 changed"), "{again:?}");
}

// Review Focus 2
#[test]
fn odd_names_link_and_select() {
    let fx = Fixture::new();
    fx.write("a b.conf", "space");
    fx.write("ünï", "unicode");
    fx.write("-rf", "dash");
    fx.ok(&["apply", "--", "-rf"]);
    assert!(is_symlink(&fx.home_path("-rf")));
    assert!(!fx.home_path("a b.conf").exists());
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path("a b.conf")), "space");
    assert_eq!(read(&fx.home_path("ünï")), "unicode");
}

// Review Focus 3
#[test]
fn subdir_cwd_paths_are_repo_relative() {
    let mut fx = flat();
    fx.env_repo = false;
    fx.write("rustow.toml", "version = 1\n");
    fx.cwd = fx.repo.join(".config");
    fx.ok(&["apply", ".bashrc"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!fx.home_path(".config/nvim/init.lua").exists());
}

// Review Focus 4
#[test]
fn ancestor_symlink_dir_is_followed_when_safe() {
    let fx = flat();
    let elsewhere = fx.base.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, fx.home_path(".config")).unwrap();
    fx.ok(&["apply"]);
    assert!(is_symlink(&elsewhere.join("nvim/init.lua")));
    assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "-- nvim\n");
    assert!(is_symlink(&fx.home_path(".config")), "the user's symlink is kept");
}

/// umask 002 with user-private groups: a group-writable symlinked ancestor
/// follows the same group rule as a link target's chain.
#[test]
fn group_writable_ancestor_symlink_follows_the_private_group_rule() {
    use nix::unistd::{Group, User, geteuid};
    use std::os::unix::fs::PermissionsExt;
    let fx = flat();
    let user = User::from_uid(geteuid()).unwrap().expect("test user");
    let group = Group::from_gid(user.gid).unwrap().expect("primary group");
    let private = group.name == user.name && group.mem.iter().all(|m| *m == user.name);
    let shared = fx.base.join("shared");
    fs::create_dir_all(&shared).unwrap();
    std::os::unix::fs::chown(&shared, None, Some(user.gid.as_raw())).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o775)).unwrap();
    std::os::unix::fs::symlink(&shared, fx.home_path(".config")).unwrap();
    let out = fx.run(&["apply"]);
    if private {
        assert_eq!(out.code, 0, "{out:?}");
        assert!(is_symlink(&shared.join("nvim/init.lua")));
    } else {
        assert_eq!(out.code, 3, "{out:?}");
        assert!(out.stdout.contains("chmod g-w"), "{out:?}");
        assert!(!shared.join("nvim").exists());
    }
}

#[test]
fn unsafe_ancestor_symlink_blocks() {
    let fx = flat();
    let open = fx.base.join("open");
    fs::create_dir_all(&open).unwrap();
    fs::set_permissions(&open, std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
    std::os::unix::fs::symlink(&open, fx.home_path(".config")).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("writable by others"), "{out:?}");
    assert!(!open.join("nvim").exists());
}

/// Review I2: without a marker, --repo or RUSTOW_REPO no directory is
/// silently taken as the repository (bare `rustow` deploys everything).
#[test]
fn no_marker_means_no_repository() {
    let mut fx = flat();
    fs::remove_file(fx.repo.join("rustow.toml")).unwrap();
    fx.env_repo = false;
    fx.cwd = fx.repo.join(".config/nvim");
    for args in [&["apply", "init.lua"][..], &[][..]] {
        let out = fx.run(args);
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stderr.contains("no rustow repository"), "{out:?}");
    }
    assert_eq!(
        fs::read_dir(&fx.home).unwrap().count(),
        1,
        "only the repository is in HOME"
    );
    let repo = fx.repo.to_str().unwrap().to_owned();
    fx.ok(&["--repo", &repo, "apply", ".config/nvim/init.lua"]);
    assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "-- nvim\n");
}

/// A problem with one source does not stop the run: a nested suffix
/// and a source symlink are skipped with a warning each, every other
/// source deploys and the exit code stays 0.
#[test]
fn sources_that_cannot_deploy_are_skipped_with_a_warning() {
    let fx = flat();
    fx.write("n.age.gpg", "x");
    std::os::unix::fs::symlink("/nonexistent", fx.repo.join(".vimrc")).unwrap();
    let out = fx.ok(&["apply"]);
    for line in [
        "warning: .vimrc: is a symbolic link; rustow deploys regular files only. Add it to .rustowignore to skip it",
        "warning: n.age.gpg: nested encryption suffixes are not supported",
    ] {
        assert!(out.stderr.contains(line), "{out:?}");
    }
    assert!(is_symlink(&fx.home_path(".bashrc")), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")), "{out:?}");
    let mut names: Vec<String> = fs::read_dir(&fx.home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [".bashrc", ".config", "dotfiles"], "nothing else is deployed");
}

/// A link the user made by hand with an absolute path to the right source
/// is rustow's link in another spelling: it is taken over, not a conflict.
#[test]
fn absolute_link_to_the_source_is_taken_over() {
    let fx = Fixture::new();
    fx.write(".gitc", "g\n");
    fx.write(".other", "o\n");
    std::os::unix::fs::symlink(fx.repo.join(".gitc"), fx.home_path(".gitc")).unwrap();
    std::os::unix::fs::symlink(fx.repo.join(".gitc"), fx.home_path(".other")).unwrap();

    let out = fx.run(&["link", "--on-conflict", "skip"]);
    assert_eq!(out.code, 3, "only .other conflicts: {out:?}");
    let gitc = out.stdout.lines().find(|l| l.contains("~/.gitc")).unwrap_or_default();
    assert!(!gitc.contains("not managed"), "{out:?}");
    assert_eq!(read(&fx.home_path(".gitc")), "g\n");
    assert_eq!(link_text(&fx.home_path(".other")), fx.repo.join(".gitc"));

    let again = fx.run(&["status", ".gitc"]);
    assert_eq!(again.code, 0, "{again:?}");
    let twice = fx.ok(&["link", ".gitc"]);
    assert!(twice.stdout.contains("0 changed, 1 unchanged"), "{twice:?}");
}

/// A recorded destination below an unreadable directory is not orphaned:
/// rustow cannot tell if its source is still there. `status` shows it as
/// SKIPPED with the reason of the directory.
#[test]
fn a_record_below_an_unreadable_directory_is_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\nfold_directories = false\n");
    fx.write("locked/x", "x\n");
    fx.write("locked/y", "y\n");
    fx.ok(&["file", "add", "locked/y"]);
    fx.ok(&["apply"]);
    let locked = fx.repo.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let status = fx.run(&["status"]);
    let apply = fx.run(&["apply"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(status.code, 0, "{status:?}");
    for (name, mode) in [("x", "link"), ("y", "copy")] {
        let row = format!("SKIPPED ~/locked/{name}  <- locked/{name} ({mode}): cannot read: Permission denied");
        assert!(status.stdout.contains(&row), "{status:?}");
    }
    assert!(status.stdout.contains("SKIPPED locked: cannot read"), "{status:?}");
    assert!(!status.stdout.contains("ORPHANED"), "{status:?}");
    assert!(!status.stderr.contains("does not exist"), "{status:?}");
    assert_eq!(apply.code, 0, "{apply:?}");
    assert!(!apply.stdout.contains("orphaned"), "{apply:?}");
}

/// An unreadable directory is one skipped entry: the other sources
/// deploy, and the directory above it does not fold.
#[test]
fn unreadable_directory_is_skipped_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    fx.write(".config/app/a.conf", "a\n");
    fx.write(".config/app/locked/x", "x\n");
    let locked = fx.repo.join(".config/app/locked");
    let set_mode = |mode| fs::set_permissions(&locked, fs::Permissions::from_mode(mode)).unwrap();
    set_mode(0o000);
    let apply = fx.run(&["apply"]);
    let check = fx.run(&["check"]);
    let status = fx.run(&["status"]);
    let arg = fx.run(&["apply", ".config/app/locked"]);
    set_mode(0o755);
    let warning = "warning: .config/app/locked: cannot read: Permission denied";
    assert_eq!(apply.code, 0, "{apply:?}");
    assert!(apply.stderr.contains(warning), "{apply:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")), "{apply:?}");
    assert!(is_symlink(&fx.home_path(".config/app/a.conf")), "{apply:?}");
    let app = fs::symlink_metadata(fx.home_path(".config/app")).unwrap();
    assert!(app.is_dir(), "no fold over an unreadable directory: {apply:?}");
    assert_eq!(check.code, 0, "{check:?}");
    assert!(check.stderr.contains(warning), "{check:?}");
    assert_eq!(status.code, 0, "{status:?}");
    assert!(
        status
            .stdout
            .contains("SKIPPED .config/app/locked: cannot read: Permission denied"),
        "{status:?}"
    );
    assert_eq!(arg.code, 2, "{arg:?}");
    assert!(
        arg.stderr.contains("source '.config/app/locked': cannot read"),
        "{arg:?}"
    );
}
