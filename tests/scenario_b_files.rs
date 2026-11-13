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
    // 1–3: register and persist, in the state directory.
    let fx = registered();
    assert!(!fx.repo.join("rustow.toml").exists());
    assert_eq!(fx.state_json()["files"], serde_json::json!([SRC]));
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
fn file_add_is_idempotent_and_leaves_the_config_alone() {
    let fx = Fixture::new();
    let config = "# my config\nversion = 1\n[vars]\nx = 1 # note\n[[files]]\nsource = \"b.conf\"\n";
    fx.write("rustow.toml", config);
    fx.write("a.conf", "a");
    fx.write("b.conf", "b");
    fx.ok(&["file", "add", "a.conf"]);
    let out = fx.ok(&["file", "add", "./a.conf", "b.conf"]);
    assert_eq!(out.stdout.matches("already registered").count(), 2, "{out:?}");
    assert_eq!(read(&fx.repo.join("rustow.toml")), config);
    assert_eq!(fx.state_json()["files"], serde_json::json!(["a.conf"]));
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.home_path("a.conf")) && is_regular(&fx.home_path("b.conf")));
}

#[test]
fn a_missing_state_registration_is_a_warning() {
    let fx = registered();
    fs::remove_file(fx.repo.join(SRC)).unwrap();
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr.contains("`rustow file add` registration does not exist"),
        "{out:?}"
    );
}

#[test]
fn file_add_rejects_ineligible_sources() {
    let fx = Fixture::new();
    fx.write(".gitconfig.tmpl", "x");
    fx.write("a.bak", "x");
    fx.write(".rustowignore", "*.bak\n");
    fx.write("dir/f", "x");
    fx.write("n.age.gpg", "x");
    std::os::unix::fs::symlink("dir/f", fx.repo.join("lnk")).unwrap();
    std::os::unix::fs::symlink("dir", fx.repo.join("ld")).unwrap();
    for (path, word) in [
        (".gitconfig.tmpl", "template"),
        ("a.bak", "ignored"),
        (".rustowignore", "reserved"),
        ("missing", "does not match"),
        ("dir", "directory"),
        ("n.age.gpg", "nested encryption suffixes"),
        ("lnk", "is a symbolic link"),
        ("ld/f", "does not match"),
    ] {
        let out = fx.run(&["file", "add", path]);
        assert_eq!(out.code, 2, "{path}: {out:?}");
        assert!(out.stderr.contains(word), "{path}: {out:?}");
    }
    assert!(!fx.repo.join("rustow.toml").exists());
}

#[test]
fn file_list_shows_registrations() {
    let fx = registered();
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \"b.conf\"\n");
    fx.write("b.conf", "b");
    let out = fx.ok(&["file", "list"]);
    assert!(out.stdout.contains(SRC) && out.stdout.contains("copy"), "{out:?}");
    assert!(out.stdout.contains(&format!("-> ~/{SRC} (copy)")), "{out:?}");
    let json = fx.ok(&["file", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(v["files"][0]["source"], SRC);
    assert_eq!(v["files"][1]["source"], "b.conf");
    let dest = fx.home_path(SRC).display().to_string();
    assert_eq!(v["files"][0]["dest"], dest.as_str(), "JSON has absolute paths");
}

#[test]
fn file_remove_keeps_deployed_copy() {
    let fx = registered();
    fx.ok(&["file", "apply"]);
    fx.ok(&["file", "remove", SRC]);
    assert_eq!(fx.state_json()["files"], serde_json::Value::Null);
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
    fx.write("rustow.toml", "version = 1\n[defaults]\nfold_directories = false\n");
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

/// `diff --json` gives one object for each differing item; exit codes stay.
#[test]
fn diff_json_lists_each_differing_item() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \".config/example/settings.json\"\n[[files]]\nsource = \"img.bin\"\n\
         [[template_rules]]\nsource = \"app.conf.tmpl\"\nmode = \"0600\"\n",
    );
    fx.write(SRC, "{\"a\": 1}\n");
    fx.write("app.conf.tmpl", "x\n");
    fx.write(".bashrc", "b\n");
    fs::write(fx.repo.join("img.bin"), [0u8, 1]).unwrap();
    fx.ok(&["apply"]);
    let json = |args: &[&str], code: i32| -> serde_json::Value {
        let out = fx.run(args);
        assert_eq!(out.code, code, "{out:?}");
        serde_json::from_str(&out.stdout).unwrap_or_else(|e| panic!("{e}: {out:?}"))
    };
    assert_eq!(json(&["diff", "--json"], 0)["items"], serde_json::json!([]));

    fs::write(fx.home_path(SRC), "{\"a\": \"mine\"}\n").unwrap();
    fs::write(fx.repo.join("img.bin"), [0u8, 2]).unwrap();
    fs::set_permissions(fx.home_path("app.conf"), fs::Permissions::from_mode(0o644)).unwrap();
    fx.write(".zshrc", "z\n");
    let v = json(&["diff", "--json"], 1);
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "the unchanged .bashrc is not listed: {v}");
    let item = |source: &str| {
        items
            .iter()
            .find(|i| i["source"] == source)
            .unwrap_or_else(|| panic!("{source}: {v}"))
    };
    let modified = item(SRC);
    assert_eq!(modified["kind"], "modified");
    assert_eq!(modified["mode"], "copy");
    assert_eq!(modified["dest"], fx.home_path(SRC).to_str().unwrap());
    assert_eq!(modified["binary"], false);
    let text = modified["diff"].as_str().unwrap();
    assert!(text.contains("-{\"a\": \"mine\"}\n+{\"a\": 1}\n"), "{v}");
    assert_eq!(item("img.bin")["kind"], "outdated");
    assert_eq!(item("img.bin")["binary"], true);
    assert_eq!(item("app.conf.tmpl")["kind"], "metadata");
    assert_eq!(item(".zshrc")["kind"], "missing");
    assert_eq!(
        json(&["file", "diff", "--json"], 1)["items"].as_array().unwrap().len(),
        2
    );

    fx.write("bad.tmpl", "{{ vars.nope }}");
    let v = json(&["diff", "--json"], 4);
    let bad = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["source"] == "bad.tmpl");
    assert_eq!(bad.map(|b| &b["kind"]), Some(&serde_json::json!("error")), "{v}");
    assert!(bad.is_some_and(|b| b["detail"].is_string()), "{v}");
    assert_eq!(bad.map(|b| &b["binary"]), Some(&serde_json::json!(false)), "{v}");
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

/// A comment above a removed `[[files]]` header may head the entries that
/// follow it; `file remove` keeps it, also when no entry is left.
#[test]
fn file_remove_keeps_comments_above_the_entry() {
    let fx = Fixture::new();
    let config =
        "version = 1\n\n# group: work files\n[[files]]\nsource = \"a\"\n\n# b note\n[[files]]\nsource = \"b\"\n";
    fx.write("rustow.toml", config);
    fx.write("a", "a");
    fx.write("b", "b");
    fx.ok(&["file", "remove", "a"]);
    let once = read(&fx.repo.join("rustow.toml"));
    assert!(
        once.contains("# group: work files") && once.contains("# b note"),
        "{once}"
    );
    assert!(!once.contains("\"a\""), "{once}");
    fx.ok(&["file", "remove", "b"]);
    let twice = read(&fx.repo.join("rustow.toml"));
    assert!(
        twice.contains("# group: work files") && twice.contains("# b note"),
        "{twice}"
    );
    assert!(!twice.contains("[[files]]"), "{twice}");
    fx.ok(&["check"]);
}

/// A `[[files]]` entry sets mode, owner and group, as a `[[template_rules]]`
/// entry does, for home and system files. Without them, the copy keeps the
/// source mode (see above).
#[test]
fn files_entry_sets_mode_owner_and_group() {
    use std::os::unix::fs::MetadataExt;
    let uid = nix::unistd::geteuid().as_raw();
    let gid = nix::unistd::getegid().as_raw();
    let group = nix::unistd::Group::from_gid(nix::unistd::getegid())
        .unwrap()
        .unwrap()
        .name;
    let config = |mode: &str| {
        format!(
            "version = 1\n[[files]]\nsource = \"{SRC}\"\nmode = \"{mode}\"\nowner = \"{uid}\"\ngroup = \"{group}\"\n\
             [[files]]\nsource = \"rustowroot/etc/app.conf\"\nmode = \"0640\"\n"
        )
    };
    let fx = Fixture::new();
    fx.write(SRC, "{}\n");
    fx.write("rustowroot/etc/app.conf", "a\n");
    fx.write("rustow.toml", &config("0600"));
    fx.ok(&["file", "apply"]);
    let dest = fx.home_path(SRC);
    assert!(is_regular(&dest));
    assert_eq!(mode(&dest), 0o600);
    let meta = fs::metadata(&dest).unwrap();
    assert_eq!((meta.uid(), meta.gid()), (uid, gid));
    assert_eq!(mode(&fx.root_path("etc/app.conf")), 0o640);
    // A metadata-only change rewrites no content.
    fx.write("rustow.toml", &config("0644"));
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("metadata"), "{out:?}");
    assert_eq!(mode(&dest), 0o644);
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
}

/// `check` finds an unknown name in a `[[files]]` entry before `apply` fails on it.
#[test]
fn check_reports_unknown_owner_in_files_entry() {
    let fx = Fixture::new();
    fx.write(SRC, "{}\n");
    fx.write(
        "rustow.toml",
        &format!("version = 1\n[[files]]\nsource = \"{SRC}\"\nowner = \"nosuchuser\"\n"),
    );
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 1, "{out:?}");
    let want = format!("rustow.toml: [[files]] source '{SRC}': unknown user 'nosuchuser'");
    assert!(out.stdout.lines().any(|l| l == format!("problem: {want}")), "{out:?}");
    assert_eq!(fx.run(&["apply"]).code, 4);
    assert!(!fx.home_path(SRC).exists());
}

/// `diff` shows content only up to 1 MiB on each side; a larger file gets
/// a summary line with both sizes.
#[test]
fn diff_summarizes_a_large_text_file() {
    let fx = Fixture::new();
    let line = "a line of text that is long enough\n";
    let big = line.repeat((2 << 20) / line.len() + 1);
    fx.write(".big", &big);
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \".big\"\n");
    let out = fx.run(&["diff"]);
    assert_eq!(out.code, 1, "{:?}", out.stderr);
    assert_eq!(
        out.stdout,
        format!("~/.big: differ (0 → {} bytes); too large to show\n", big.len())
    );
    let json: serde_json::Value = serde_json::from_str(&fx.run(&["diff", "--json"]).stdout).unwrap();
    let item = &json["items"][0];
    assert_eq!(item["binary"], false, "a large text file is not binary: {item}");
    assert!(
        item["diff"].as_str().unwrap().ends_with("too large to show\n"),
        "{item}"
    );
    fx.ok(&["apply"]);
    let same = fx.run(&["diff"]);
    assert_eq!((same.code, same.stdout.as_str()), (0, ""), "equal content is not read");
}

/// A deployed copy whose source is removed is orphaned, as a link or a
/// template is: the registration still names it, but it is not a source.
#[test]
fn removed_source_of_a_registered_copy_is_orphaned() {
    for config in ["", "version = 1\n[[files]]\nsource = \".d\"\n"] {
        let fx = Fixture::new();
        fx.write(".d", "d\n");
        match config {
            "" => drop(fx.ok(&["file", "add", ".d"])),
            text => drop(fx.write("rustow.toml", text)),
        }
        fx.ok(&["apply"]);
        fs::remove_file(fx.repo.join(".d")).unwrap();
        let status = fx.run(&["status"]);
        assert_eq!(status.code, 1, "{config:?}\n{status:?}");
        assert!(
            status.stdout.contains("ORPHANED ~/.d  <- .d (copy): source removed"),
            "{status:?}"
        );
        assert!(
            status.stdout.contains("does not exist"),
            "the warning row stays: {status:?}"
        );
        let apply = fx.ok(&["apply"]);
        assert!(apply.stdout.contains("orphaned ~/.d  <- .d (copy)"), "{apply:?}");
        assert!(apply.stderr.contains("warning: .d: "), "{apply:?}");
        assert_eq!(read(&fx.home_path(".d")), "d\n", "rustow removes nothing");
    }
}

/// A registered copy whose source is now ignored or a directory is
/// orphaned, as a link in the same case is. rustow removes nothing.
#[test]
fn registered_copy_whose_path_is_no_source_is_orphaned() {
    for (change, why) in [
        ("ignored", "source is ignored by .rustowignore"),
        ("a directory", "source is a directory, not a file"),
    ] {
        let fx = Fixture::new();
        fx.write(".d", "d\n");
        fx.write(".l", "l\n");
        fx.ok(&["file", "add", ".d"]);
        fx.ok(&["apply"]);
        match change {
            "ignored" => drop(fx.write(".rustowignore", ".d\n.l\n")),
            _ => {
                fs::remove_file(fx.repo.join(".d")).unwrap();
                fs::remove_file(fx.repo.join(".l")).unwrap();
                fx.write(".d/inner", "i\n");
                fx.write(".l/inner", "i\n");
            }
        }
        let status = fx.run(&["status"]);
        assert_eq!(status.code, 1, "{change}: {status:?}");
        for (name, mode) in [(".d", "copy"), (".l", "link")] {
            let row = format!("ORPHANED ~/{name}  <- {name} ({mode}): {why}; `rustow undeploy {name}` removes it");
            assert!(status.stdout.contains(&row), "{change}: {status:?}");
        }
        let apply = fx.run(&["apply"]);
        assert!(
            apply.stdout.contains("orphaned ~/.d  <- .d (copy)"),
            "{change}: {apply:?}"
        );
        assert_eq!(read(&fx.home_path(".d")), "d\n", "rustow removes nothing");
    }
}
