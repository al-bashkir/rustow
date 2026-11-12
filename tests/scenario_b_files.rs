//! Spec §17.2 Scenario B — registered files — plus diff behavior.

mod common;

use common::*;
use std::fs;

const SRC: &str = ".config/example/settings.json";

fn registered() -> Fixture {
    let fx = Fixture::new();
    fx.write(SRC, "{\"a\": 1}\n");
    fx.ok(&["file", "add", SRC]);
    fx
}

#[test]
fn scenario_b_full_flow() {
    // 1–3: register and persist.
    let fx = registered();
    let config = read(&fx.repo.join(".rustow.toml"));
    assert!(
        config.contains("[[files]]\nsource = \".config/example/settings.json\""),
        "{config}"
    );
    // Registration deploys nothing.
    assert!(!fx.home_path(SRC).exists());
    // 4–5: link skips registered files.
    fx.ok(&["link"]);
    assert!(!fx.home_path(SRC).exists());
    // 6–7: file apply copies a regular file.
    fx.ok(&["file", "apply"]);
    assert!(is_regular(&fx.home_path(SRC)));
    assert_eq!(read(&fx.home_path(SRC)), "{\"a\": 1}\n");
    // 8–9: source change shows in diff, exit 1.
    fx.write(SRC, "{\"a\": 2}\n");
    let diff = fx.run(&["file", "diff"]);
    assert_eq!(diff.code, 1, "{diff:?}");
    assert!(
        diff.stdout.contains("-{\"a\": 1}") && diff.stdout.contains("+{\"a\": 2}"),
        "{diff:?}"
    );
    // 10: safe update.
    let out = fx.ok(&["file", "apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert_eq!(read(&fx.home_path(SRC)), "{\"a\": 2}\n");
    assert_eq!(fx.run(&["file", "diff"]).code, 0);
    // 11–12: independent destination change is a conflict, never overwritten.
    fs::write(fx.home_path(SRC), "{\"a\": \"mine\"}\n").unwrap();
    let out = fx.run(&["file", "apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("modified since rustow last applied"), "{out:?}");
    assert_eq!(read(&fx.home_path(SRC)), "{\"a\": \"mine\"}\n");
    // Both changed: still a conflict.
    fx.write(SRC, "{\"a\": 3}\n");
    assert_eq!(fx.run(&["apply"]).code, 3);
    // Explicit backup resolves it.
    fx.ok(&["file", "apply", "--backup"]);
    assert_eq!(read(&fx.home_path(SRC)), "{\"a\": 3}\n");
}

#[test]
fn file_add_is_idempotent_and_keeps_comments() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "# my config\nversion = 1\n[vars]\nx = 1 # note\n");
    fx.write("a.conf", "a");
    fx.ok(&["file", "add", "a.conf"]);
    let once = read(&fx.repo.join(".rustow.toml"));
    let out = fx.ok(&["file", "add", "./a.conf"]);
    assert!(out.stdout.contains("already registered"), "{out:?}");
    assert_eq!(read(&fx.repo.join(".rustow.toml")), once);
    assert!(
        once.starts_with("# my config\nversion = 1\n[vars]\nx = 1 # note\n"),
        "{once}"
    );
    assert_eq!(once.matches("[[files]]").count(), 1);
}

#[test]
fn file_add_rejects_ineligible_sources() {
    let fx = Fixture::new();
    fx.write(".gitconfig.tmpl", "x");
    fx.write("a.bak", "x");
    fx.write(".rustowignore", "*.bak\n");
    fx.write("dir/f", "x");
    for (path, word) in [
        (".gitconfig.tmpl", "template"),
        ("a.bak", "ignored"),
        (".rustowignore", "reserved"),
        ("missing", "does not match"),
        ("dir", "directory"),
    ] {
        let out = fx.run(&["file", "add", path]);
        assert_eq!(out.code, 2, "{path}: {out:?}");
        assert!(out.stderr.contains(word), "{path}: {out:?}");
    }
    assert!(!fx.repo.join(".rustow.toml").exists());
}

#[test]
fn file_list_shows_registrations() {
    let fx = registered();
    let out = fx.ok(&["file", "list"]);
    assert!(out.stdout.contains(SRC) && out.stdout.contains("copy"), "{out:?}");
    let json = fx.ok(&["file", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(v["files"][0]["source"], SRC);
}

#[test]
fn file_remove_keeps_deployed_copy() {
    let fx = registered();
    fx.ok(&["file", "apply"]);
    fx.ok(&["file", "remove", SRC]);
    assert!(!read(&fx.repo.join(".rustow.toml")).contains("[[files]]"));
    assert!(is_regular(&fx.home_path(SRC)), "the deployed copy stays");
    let out = fx.ok(&["file", "remove", SRC]);
    assert!(out.stdout.contains("not registered"), "{out:?}");
}

#[test]
fn copy_to_symlink_after_remove_is_conflict_until_backup() {
    let fx = registered();
    fx.ok(&["file", "apply"]);
    fx.ok(&["file", "remove", SRC]);
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        out.stdout.contains("managed copy would be replaced by a symlink"),
        "{out:?}"
    );
    assert!(is_regular(&fx.home_path(SRC)));
    fx.ok(&["apply", "--backup"]);
    assert!(is_symlink(&fx.home_path(SRC)));
}

#[test]
fn symlink_to_copy_transition_is_automatic() {
    let fx = Fixture::new();
    fx.write(SRC, "x\n");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(SRC)));
    fx.ok(&["file", "add", SRC]);
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert!(is_regular(&fx.home_path(SRC)));
}

#[test]
fn copy_keeps_source_mode_then_destination_mode() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    let src = fx.write("bin/tool", "#!/bin/sh\n");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    fx.ok(&["file", "add", "bin/tool"]);
    fx.ok(&["apply"]);
    assert_eq!(mode(&fx.home_path("bin/tool")), 0o755);
    fs::set_permissions(fx.home_path("bin/tool"), fs::Permissions::from_mode(0o700)).unwrap();
    fx.write("bin/tool", "#!/bin/sh\necho 2\n");
    fx.ok(&["apply"]);
    assert_eq!(
        mode(&fx.home_path("bin/tool")),
        0o700,
        "an update keeps the destination mode"
    );
}

#[test]
fn identical_unmanaged_copy_is_adopted() {
    let fx = registered();
    fx.write_home(SRC, "{\"a\": 1}\n");
    let out = fx.ok(&["file", "apply"]);
    assert!(out.stdout.contains("adopt"), "{out:?}");
}

#[test]
fn diff_exit_codes_and_scope() {
    let fx = registered();
    fx.write(".bashrc", "b\n");
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout.contains("--- /dev/null") && out.stdout.contains("+{\"a\": 1}"),
        "{out:?}"
    );
    assert!(
        out.stdout.contains(".bashrc"),
        "symlinks show in top-level diff: {out:?}"
    );
    let files_only = fx.run(&["file", "diff"]);
    assert!(!files_only.stdout.contains(".bashrc"), "{files_only:?}");
    fx.ok(&["apply"]);
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, "");
    let one = fx.run(&["diff", SRC]);
    assert_eq!(one.code, 0, "{one:?}");
}

#[test]
fn binary_diff_message() {
    let fx = Fixture::new();
    fs::write(fx.repo.join("img.bin"), [0u8, 1, 2, 3]).unwrap();
    fx.ok(&["file", "add", "img.bin"]);
    fx.ok(&["apply"]);
    fs::write(fx.repo.join("img.bin"), [0u8, 9, 9, 9]).unwrap();
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("Binary files"), "{out:?}");
}

/// Spec §8.5: when the source and the destination both changed, status and
/// diff show a conflict, not a plain local modification.
#[test]
fn both_changed_is_a_conflict_in_status_and_diff() {
    let fx = registered();
    fx.write(".gitconfig.tmpl", "x\n");
    fx.ok(&["apply"]);
    fs::write(fx.home_path(SRC), "{\"a\": \"mine\"}\n").unwrap();
    // Writes through the link into the cache entry.
    fs::write(fx.home_path(".gitconfig"), "mine\n").unwrap();
    let status = fx.run(&["status"]);
    for name in [SRC, ".gitconfig"] {
        assert!(
            status
                .stdout
                .lines()
                .any(|l| l.contains("MODIFIED") && l.contains(name)),
            "{status:?}"
        );
    }
    fx.write(SRC, "{\"a\": 2}\n");
    fx.write(".gitconfig.tmpl", "y\n");
    let status = fx.run(&["status"]);
    assert_eq!(status.code, 1, "{status:?}");
    for name in [SRC, ".gitconfig"] {
        let line = status.stdout.lines().find(|l| l.contains(name)).unwrap_or_default();
        assert!(
            line.contains("CONFLICT") && line.contains("source and destination both changed"),
            "{status:?}"
        );
    }
    let diff = fx.run(&["diff"]);
    assert_eq!(diff.code, 1, "{diff:?}");
    for name in [SRC, ".gitconfig"] {
        assert!(
            diff.stdout
                .contains(&format!("~/{name}: conflict: source and destination both changed")),
            "{diff:?}"
        );
    }
    assert!(diff.stdout.contains("+{\"a\": 2}"), "{diff:?}");
}

#[test]
fn file_apply_rejects_non_registered_path() {
    let fx = registered();
    fx.write(".bashrc", "b");
    let out = fx.run(&["file", "apply", ".bashrc"]);
    assert_eq!(out.code, 2, "{out:?}");
}
