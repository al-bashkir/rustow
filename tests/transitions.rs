//! Spec §11.1 — mode transitions must not destroy independent changes.

mod common;

use common::*;
use std::fs;

fn files_below(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_below(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// An edit written through a cache-backed link must survive the switch
/// to a copy: a conflict first, then `--backup` saves the edit.
fn assert_edit_survives_switch_to_copy(fx: &Fixture, dest: &str, switch: impl Fn()) {
    let path = fx.home_path(dest);
    assert!(is_symlink(&path));
    fs::write(&path, "EDIT\n").unwrap();
    switch();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("conflict"), "{out:?}");
    assert_eq!(read(&path), "EDIT\n", "nothing changed");
    let out = fx.ok(&["apply", "--backup"]);
    assert!(out.stdout.contains("backup"), "{out:?}");
    assert!(is_regular(&path));
    let backups = files_below(&fx.state.join("rustow/backups"));
    assert!(
        backups
            .iter()
            .any(|b| fs::read_to_string(b).is_ok_and(|s| s == "EDIT\n")),
        "the edit is saved: {backups:?}"
    );
}

#[test]
fn template_link_to_copy_keeps_an_edit_made_through_the_link() {
    let fx = Fixture::new();
    fx.write(".t.tmpl", "rendered\n");
    fx.ok(&["apply"]);
    assert_edit_survives_switch_to_copy(&fx, ".t", || {
        fx.write(
            ".rustow.toml",
            "version = 1\n[[template_rules]]\nsource = \".t.tmpl\"\nmode = \"0644\"\n",
        );
    });
    assert_eq!(read(&fx.home_path(".t")), "rendered\n");
}

/// A cache entry that only feeds a copy is not guarded: a change made to
/// it outside rustow is refreshed, not reported as a user edit.
#[test]
fn copied_template_refreshes_a_changed_cache_entry() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[[template_rules]]\nsource = \".t.tmpl\"\nmode = \"0644\"\n",
    );
    fx.write(".t.tmpl", "rendered\n");
    fx.ok(&["apply"]);
    let cache = files_below(&fx.repo.join(".rustowcache/rendered"));
    assert_eq!(cache.len(), 1, "{cache:?}");
    fs::write(&cache[0], "stale\n").unwrap();
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("1 changed"), "{out:?}");
    assert_eq!(read(&cache[0]), "rendered\n");
    assert_eq!(read(&fx.home_path(".t")), "rendered\n");
}

#[test]
fn decrypt_link_to_copy_keeps_an_edit_made_through_the_link() {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[&key], &[&key], "");
    fx.age_encrypt("x.age", "secret\n", &[&key]);
    fx.ok(&["apply"]);
    assert_edit_survives_switch_to_copy(&fx, "x", || {
        fx.ok(&["file", "add", "x.age"]);
    });
    assert_eq!(read(&fx.home_path("x")), "secret\n");
}

#[test]
fn unedited_template_link_switches_to_copy_without_conflict() {
    let fx = Fixture::new();
    fx.write(".t.tmpl", "rendered\n");
    fx.ok(&["apply"]);
    fx.write(
        ".rustow.toml",
        "version = 1\n[[template_rules]]\nsource = \".t.tmpl\"\nmode = \"0600\"\n",
    );
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert!(is_regular(&fx.home_path(".t")));
    assert_eq!(mode(&fx.home_path(".t")), 0o600);
    assert_eq!(read(&fx.home_path(".t")), "rendered\n");
}
