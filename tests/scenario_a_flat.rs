//! Spec §17.2 Scenario A — flat repository — plus Review Focus 1–4.

mod common;

use common::*;
use std::fs;
use std::path::PathBuf;

fn flat() -> Fixture {
    let fx = Fixture::new();
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
    assert_eq!(
        link_text(&fx.home_path(".bashrc")),
        PathBuf::from("dotfiles/.bashrc")
    );
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
    assert!(
        meta.is_dir(),
        "parent must be a real directory, not a folded link"
    );
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
    fx.write(".rustow.toml", "version = 1\n");
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
    assert!(
        is_symlink(&fx.home_path(".config")),
        "the user's symlink is kept"
    );
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
