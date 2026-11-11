//! Diff, doctor, init and unfold paths that the scenario tests do not reach.

mod common;

use common::*;
use std::fs;

#[test]
fn diff_reports_render_failure_and_exits_4() {
    let fx = Fixture::new();
    fx.write(".profile.tmpl", "{{ vars.missing }}\n");
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(
        out.stderr.contains("cannot compute desired content") && out.stderr.contains(".profile.tmpl"),
        "{out:?}"
    );
}

#[test]
fn diff_reports_collisions() {
    let fx = Fixture::new();
    fx.write("x", "plain");
    fx.write("x.tmpl", "tmpl");
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout.contains("several sources map to this destination"),
        "{out:?}"
    );
}

#[test]
fn diff_describes_what_is_in_the_way_of_a_link() {
    let fx = Fixture::new();
    fx.write("file", "f");
    fx.write("dir", "d");
    fx.write("dangling", "x");
    fx.write_home("file", "mine");
    fs::create_dir(fx.home_path("dir")).unwrap();
    std::os::unix::fs::symlink("nowhere", fx.home_path("dangling")).unwrap();
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("~/file: regular file; would link to"), "{out:?}");
    assert!(
        out.stdout.contains("~/dir: a directory is in the way") || out.stdout.contains("~/dir: directory;"),
        "{out:?}"
    );
    assert!(
        out.stdout.contains("~/dangling: links to nowhere; would link to"),
        "{out:?}"
    );
}

#[test]
fn diff_describes_a_symlink_in_place_of_a_copy() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n[[files]]\nsource = \"c\"\n");
    fx.write("c", "copy\n");
    std::os::unix::fs::symlink("elsewhere", fx.home_path("c")).unwrap();
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout
            .contains("~/c: links to elsewhere; would become a regular file"),
        "{out:?}"
    );
}

#[test]
fn init_json_lists_changes() {
    let fx = Fixture::new();
    let out = fx.ok(&["init", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["repository"], fx.repo.to_str().unwrap());
    let changes = v["changes"].as_array().unwrap();
    assert!(
        changes
            .iter()
            .any(|c| c["path"] == ".rustow.toml" && c["action"] == "create"),
        "{v}"
    );
    assert_eq!(v["dry_run"], false);
}

#[test]
fn doctor_flags_unusable_gpg_home_and_json() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[encryption.gpg]\nrecipients = [\"70724AA6092233B04A3002C3F3E77E27DD87359D\"]\nhomedir = \"/nonexistent/gnupg\"\n",
    );
    let out = fx.run(&["doctor"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("gpg home"), "{out:?}");
    let json = fx.run(&["doctor", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert!(
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["check"] == "gpg home" && c["level"] == "fail")
    );
}

#[test]
fn doctor_flags_relative_identity_path() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[encryption]\nidentity_files = [\"keys/id.txt\"]\n",
    );
    let out = fx.run(&["doctor"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("must be absolute"), "{out:?}");
}

#[test]
fn replaced_folded_link_becomes_the_users_own_directory() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n[defaults]\nfold_directories = true\n");
    fx.write(".config/nvim/init.lua", "x");
    fs::create_dir_all(fx.home_path(".config")).unwrap();
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")));
    // The user points the folded link elsewhere, then folding is turned off.
    let theirs = fx.base.join("theirs");
    fs::create_dir(&theirs).unwrap();
    fs::remove_file(fx.home_path(".config/nvim")).unwrap();
    std::os::unix::fs::symlink(&theirs, fx.home_path(".config/nvim")).unwrap();
    fx.write(".rustow.toml", "version = 1\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        is_symlink(&theirs.join("init.lua")),
        "deployed into the user's directory: {out:?}"
    );
    assert_eq!(link_text(&fx.home_path(".config/nvim")), theirs);
}

fn summary(out: &Out) -> &str {
    out.stdout.lines().last().unwrap_or_default()
}

#[test]
fn summary_lines_are_exact() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b");
    fx.write(".zshrc", "z");
    fx.ok(&["apply"]);
    assert_eq!(summary(&fx.ok(&["apply"])), "0 changed, 2 unchanged");
    fx.write_home("new", "mine");
    fx.write("new", "repo");
    let out = fx.run(&["apply"]);
    assert_eq!(
        summary(&out),
        "conflicts found, nothing written: 0 to change, 2 unchanged, 1 conflicts"
    );
    fx.write("bad.tmpl", "{{ vars.nope }}");
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(
        summary(&out),
        "0 changed, 2 unchanged, 1 conflicts, 1 errors",
        "{out:?}"
    );
}

#[test]
fn doctor_checks_gpg_home_when_only_homedir_is_set() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[encryption.gpg]\nhomedir = \"/nonexistent/gnupg\"\n",
    );
    let out = fx.run(&["doctor"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("gpg home"), "{out:?}");
}
