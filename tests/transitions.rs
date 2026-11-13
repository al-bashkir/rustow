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
            "rustow.toml",
            "version = 1\n[[template_rules]]\nsource = \".t.tmpl\"\nmode = \"0644\"\n",
        );
    });
    assert_eq!(read(&fx.home_path(".t")), "rendered\n");
}

/// A template+copy or decrypt+copy writes no cache entry: no plaintext
/// stays in the cache, and `cache clean` leaves nothing for the next run
/// to rewrite.
#[test]
fn a_copy_writes_no_cache_entry() {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    let rule = "[[template_rules]]\nsource = \".t.tmpl\"\nmode = \"0644\"\n";
    fx.age_config(&[&key], &[&key], rule);
    fx.write(".t.tmpl", "rendered\n");
    fx.age_encrypt("x.age", "secret\n", &[&key]);
    fx.ok(&["file", "add", "x.age"]);
    fx.ok(&["apply"]);
    assert_eq!(
        files_below(&fx.repo.join(".rustowcache")),
        Vec::<std::path::PathBuf>::new()
    );
    fx.ok(&["cache", "clean"]);
    let copies = [fx.home_path(".t"), fx.home_path("x")];
    let before: Vec<_> = copies.iter().map(|p| mtime(p)).collect();
    let status = fx.run(&["status"]);
    assert_eq!(status.code, 0, "{status:?}");
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("0 changed"), "{out:?}");
    assert_eq!(copies.iter().map(|p| mtime(p)).collect::<Vec<_>>(), before);
    assert_eq!(read(&copies[1]), "secret\n");
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
        "rustow.toml",
        "version = 1\n[[template_rules]]\nsource = \".t.tmpl\"\nmode = \"0600\"\n",
    );
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert!(is_regular(&fx.home_path(".t")));
    assert_eq!(mode(&fx.home_path(".t")), 0o600);
    assert_eq!(read(&fx.home_path(".t")), "rendered\n");
}
