//! Spec §17.2 Scenario J — selective commands, plus status, check,
//! doctor, cache clean and undeploy.

mod common;

use common::*;
use std::fs;

fn mixed() -> Fixture {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[vars]\nv = \"t\"\n[[files]]\nsource = \"copied.conf\"\n",
    );
    fx.write(".bashrc", "b\n");
    fx.write("copied.conf", "c\n");
    fx.write(".gitconfig.tmpl", "{{ vars.v }}\n");
    fx
}

fn deployed(fx: &Fixture) -> [bool; 3] {
    [".bashrc", "copied.conf", ".gitconfig"].map(|f| fs::symlink_metadata(fx.home_path(f)).is_ok())
}

#[test]
fn each_command_deploys_only_its_modes() {
    let fx = mixed();
    fx.ok(&["link"]);
    assert_eq!(deployed(&fx), [true, false, false]);
    let fx = mixed();
    fx.ok(&["file", "apply"]);
    assert_eq!(deployed(&fx), [false, true, false]);
    let fx = mixed();
    fx.ok(&["template", "apply"]);
    assert_eq!(deployed(&fx), [false, false, true]);
    let fx = mixed();
    fx.ok(&["apply"]);
    assert_eq!(deployed(&fx), [true, true, true]);
}

fn status(fx: &Fixture) -> Out {
    fx.run(&["status", "-v"])
}

fn line_for<'a>(out: &'a Out, needle: &str) -> &'a str {
    out.stdout
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line for {needle}: {out:?}"))
}

#[test]
fn status_labels_and_exit_codes() {
    let fx = mixed();
    let before = status(&fx);
    assert_eq!(before.code, 1, "{before:?}");
    assert!(line_for(&before, "~/.bashrc").contains("MISSING"));
    fx.ok(&["apply"]);
    let after = status(&fx);
    assert_eq!(after.code, 0, "{after:?}");
    assert!(after.stdout.lines().any(|l| l == "3 ok, 0 need attention"), "{after:?}");
    assert!(line_for(&after, "~/.bashrc").contains("LINKED"));
    assert!(line_for(&after, "~/copied.conf").contains("COPIED"));
    assert!(line_for(&after, "~/.gitconfig").contains("TEMPLATED"));
    assert_eq!(
        fx.run(&["status"])
            .stdout
            .lines()
            .filter(|l| l.contains("LINKED"))
            .count(),
        0,
        "clean items hidden"
    );

    fx.write("copied.conf", "c2\n");
    fx.write(
        "rustow.toml",
        "version = 1\n[vars]\nv = \"t2\"\n[[files]]\nsource = \"copied.conf\"\n",
    );
    let s = status(&fx);
    assert!(line_for(&s, "~/copied.conf").contains("OUTDATED"), "{s:?}");
    assert!(line_for(&s, "~/.gitconfig").contains("OUTDATED"), "{s:?}");
    fx.ok(&["apply"]);

    fs::write(fx.home_path("copied.conf"), "edited\n").unwrap();
    assert!(line_for(&status(&fx), "~/copied.conf").contains("MODIFIED"));
    fx.ok(&["apply", "--force"]);

    fs::remove_file(fx.repo.join(".bashrc")).unwrap();
    let s = status(&fx);
    assert!(line_for(&s, "~/.bashrc").contains("ORPHANED"), "{s:?}");
}

#[test]
fn status_conflict_unmanaged_and_error() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    fx.write(".zshrc", "z\n");
    fx.write(".profile.tmpl", "{{ vars.nope }}\n");
    fx.write_home(".bashrc", "mine\n");
    std::os::unix::fs::symlink("dotfiles/.zshrc", fx.home_path(".zshrc")).unwrap();
    let s = status(&fx);
    assert_eq!(s.code, 1);
    assert!(line_for(&s, "~/.bashrc").contains("CONFLICT"), "{s:?}");
    assert!(line_for(&s, "~/.zshrc").contains("UNMANAGED"), "{s:?}");
    assert!(line_for(&s, "~/.profile").contains("ERROR"), "{s:?}");
}

#[test]
fn status_json_has_no_content() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \"secret.conf\"\n");
    fx.write("secret.conf", "s3cret-value\n");
    fx.ok(&["apply"]);
    fx.write("secret.conf", "n3w-s3cret\n");
    let out = fx.run(&["status", "--json"]);
    assert!(!out.stdout.contains("s3cret"), "{out:?}");
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["items"][0]["status"], "OUTDATED");
}

#[test]
fn check_validates_offline() {
    let fx = mixed();
    assert_eq!(fx.run(&["check"]).code, 0);
    fx.write(".ssh/x", "plain");
    fx.write(".ssh/x.age", "not real ciphertext");
    fx.write(".profile.tmpl", "{{ vars.missing }}\n");
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout.contains(".ssh/x.age") && out.stdout.contains(".profile.tmpl"),
        "{out:?}"
    );
    fx.write("rustow.toml", "version = 1\nbad = 1\n");
    assert_eq!(fx.run(&["check"]).code, 2);
}

#[test]
fn doctor_reports_environment() {
    let fx = mixed();
    fx.ok(&["init"]);
    let out = fx.run(&["doctor"]);
    assert_eq!(out.code, 0, "{out:?}");
    for word in ["repository", "cache", "state", "target"] {
        assert!(out.stdout.contains(word), "{word}: {out:?}");
    }
}

#[test]
fn cache_clean_keeps_live_entries() {
    let fx = mixed();
    fx.ok(&["apply"]);
    let live = fx.home_path(".gitconfig");
    let cache_root = fx.repo.join(".rustowcache/rendered");
    let repo_dir = fs::read_dir(&cache_root).unwrap().next().unwrap().unwrap().path();
    let scope_dir = fs::read_dir(&repo_dir).unwrap().next().unwrap().unwrap().path();
    write_file(&scope_dir.join("stale/old.conf"), "stale");
    let out = fx.ok(&["cache", "clean"]);
    assert!(out.stdout.contains("stale/old.conf"), "{out:?}");
    assert!(!scope_dir.join("stale").exists());
    assert_eq!(read(&live), "t\n", "live entry kept");
    assert!(fx.ok(&["cache", "clean"]).stdout.contains("0 changed"));
}

#[test]
fn undeploy_removes_managed_and_refuses_modified() {
    let fx = mixed();
    fx.ok(&["apply"]);
    fs::write(fx.home_path("copied.conf"), "edited\n").unwrap();
    let out = fx.run(&["undeploy", "--yes"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!fx.home_path(".bashrc").exists());
    assert!(fs::symlink_metadata(fx.home_path(".gitconfig")).is_err());
    assert_eq!(read(&fx.home_path("copied.conf")), "edited\n");
}

/// The user replaced the directory of a deployed copy with a symlink into
/// the repository: the recorded path now reaches the source itself.
/// undeploy forgets the record and leaves the source.
#[test]
fn undeploy_never_removes_a_source_through_a_symlink_into_the_repository() {
    let fx = Fixture::new();
    fx.write(".config/app/conf", "precious\n");
    fx.ok(&["file", "add", ".config/app/conf"]);
    fx.ok(&["apply"]);
    let app = fx.home_path(".config/app");
    fs::remove_dir_all(&app).unwrap();
    std::os::unix::fs::symlink(fx.repo.join(".config/app"), &app).unwrap();

    let out = fx.run(&["undeploy", "--yes"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(read(&fx.repo.join(".config/app/conf")), "precious\n", "{out:?}");
    assert!(fs::symlink_metadata(&app).unwrap().is_symlink(), "left in place");
    assert!(fx.ok(&["undeploy", "--yes"]).stdout.contains("0 changed"), "forgotten");
}

#[test]
fn undeploy_all_without_terminal_needs_yes() {
    let fx = mixed();
    fx.ok(&["apply"]);
    let out = fx.run(&["undeploy"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("--yes"), "{out:?}");
    assert_eq!(deployed(&fx), [true, true, true], "nothing removed");
    fx.ok(&["undeploy", "-y"]);
    assert_eq!(deployed(&fx), [false, false, false]);
}

#[test]
fn undeploy_all_on_a_terminal_removes_only_after_yes() {
    let fx = mixed();
    fx.ok(&["apply"]);
    let out = fx.run_tty(&["undeploy"], "n\n");
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stderr.contains("  remove ~/.bashrc  <- .bashrc (link)"), "{out:?}");
    assert!(out.stderr.contains("Remove 3 destinations? [y/N]"), "{out:?}");
    assert!(out.stderr.contains("cancelled, nothing removed"), "{out:?}");
    assert_eq!(deployed(&fx), [true, true, true]);
    let out = fx.run_tty(&["undeploy"], "yes\n");
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(deployed(&fx), [false, false, false]);

    let one = Fixture::new();
    one.write(".bashrc", "b\n");
    one.ok(&["apply"]);
    let out = one.run_tty(&["undeploy"], "\n");
    assert!(out.stderr.contains("Remove 1 destination? [y/N]"), "{out:?}");
    assert!(one.home_path(".bashrc").exists(), "an empty answer is no");
}

#[test]
fn undeploy_asks_only_for_a_full_real_run() {
    let fx = mixed();
    fx.ok(&["undeploy"]); // nothing recorded: nothing to confirm
    fx.ok(&["apply"]);
    assert!(fx.ok(&["undeploy", "-n"]).stdout.contains("dry run"));
    fx.ok(&["undeploy", ".bashrc"]);
    assert_eq!(deployed(&fx), [false, true, true]);
}
