//! A moved repository: rustow finds the state of the old path, and
//! `rustow state adopt <OLD_ROOT>` takes it over.

mod common;

use common::*;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

/// Applies a link, a template link, a decrypt link and a registered copy,
/// then moves the repository to `~/dotfiles2`. Returns the old root.
fn moved() -> (Fixture, PathBuf) {
    let mut fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[&key], &[&key], "");
    fx.age_encrypt("s.age", "s\n", &[&key]);
    fx.write(".bashrc", "b\n");
    fx.write("a.conf", "a\n");
    fx.write(".t.tmpl", "t\n");
    fx.ok(&["file", "add", "a.conf"]);
    fx.ok(&["apply"]);
    let old = fx.repo.clone();
    let new = fx.home.join("dotfiles2");
    fs::rename(&old, &new).unwrap();
    (fx.repo, fx.cwd) = (new.clone(), new);
    (fx, old)
}

/// The state file whose recorded root is `root`.
fn state_file(fx: &Fixture, root: &std::path::Path) -> PathBuf {
    let files = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path());
    files
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .find(|p| serde_json::from_str::<serde_json::Value>(&read(p)).unwrap()["repo"] == json!(root))
        .unwrap()
}

#[test]
fn state_adopt_takes_over_a_moved_repository() {
    let (fx, old) = moved();
    let old_root = old.to_str().unwrap();
    let status = fx.run(&["status"]);
    let notice = format!(
        "note: the state of a moved repository was found: {old_root}; run `rustow state adopt {old_root}` to take it over"
    );
    assert!(status.stderr.contains(&notice), "{status:?}");
    let copied = mtime(&fx.home_path("a.conf"));

    let dry = fx.ok(&["state", "adopt", "-n", old_root]);
    assert!(dry.stdout.contains("adopt"), "{dry:?}");
    assert!(
        dry.stdout
            .ends_with("\ndry run, nothing written: 1 to change, 0 unchanged\n"),
        "as other dry runs: {dry:?}"
    );
    assert!(
        fx.run(&["status"]).stderr.contains(&notice),
        "a dry run changes nothing"
    );

    fx.ok(&["state", "adopt", old_root]);
    assert_eq!(fx.state_json()["files"], json!(["a.conf"]), "registrations are kept");
    let status = fx.run(&["status"]);
    assert!(!status.stderr.contains("moved repository"), "{status:?}");
    // The records name the moved cache entry: an edit made there through the link stays.
    let clean = fx.ok(&["cache", "clean"]);
    assert!(!clean.stdout.contains("remove"), "{clean:?}");
    let out = fx.ok(&["apply"]);
    assert!(!out.stdout.contains("conflict"), "{out:?}");
    for (name, content) in [(".bashrc", "b\n"), (".t", "t\n"), ("s", "s\n")] {
        let path = fx.home_path(name);
        assert!(is_symlink(&path), "{name}: {out:?}");
        assert!(fs::canonicalize(&path).unwrap().starts_with(&fx.repo), "{name}");
        assert_eq!(read(&path), content);
    }
    for kind in ["rendered", "decrypted"] {
        let ids = fs::read_dir(fx.repo.join(".rustowcache").join(kind)).unwrap().count();
        assert_eq!(ids, 1, "{kind}: the cache entries moved to the new repository id");
    }
    assert!(is_regular(&fx.home_path("a.conf")));
    assert_eq!(mtime(&fx.home_path("a.conf")), copied, "the copy is unchanged");
    assert_eq!(fx.ok(&["status"]).code, 0);

    let backups = fx.state.join("rustow/backups");
    let kept: Vec<_> = fs::read_dir(&backups).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let adopted = kept[0].join("state-adopted.json");
    assert_eq!(mode(&adopted), 0o600);
    let json: serde_json::Value = serde_json::from_str(&read(&adopted)).unwrap();
    assert_eq!(json["repo"], json!(old_root));
    let old_id = kept[0].file_name().unwrap().to_str().unwrap().to_owned();
    let old_lock = fx.state.join("rustow").join(format!("{old_id}.lock"));
    assert!(!old_lock.exists(), "the lock of the old repository goes too");
}

#[test]
fn state_adopt_refuses_what_it_cannot_take_over() {
    let (fx, old) = moved();
    let old_root = old.to_str().unwrap();
    let new_root = fx.repo.to_str().unwrap().to_owned();
    for (arg, word) in [
        (new_root.as_str(), "still exists"),
        ("/nowhere/repo", "no state of a repository at /nowhere/repo"),
    ] {
        let out = fx.run(&["state", "adopt", arg]);
        assert_eq!(out.code, 2, "{arg}: {out:?}");
        assert!(out.stderr.contains(word), "{arg}: {out:?}");
    }

    fx.ok(&["file", "add", "a.conf"]);
    let out = fx.run(&["state", "adopt", old_root]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("this repository already has a state"), "{out:?}");

    fs::remove_file(state_file(&fx, &fx.repo)).unwrap();
    fs::write(state_file(&fx, &old), "{not json").unwrap();
    let out = fx.run(&["state", "adopt", old_root]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("move it away"), "{out:?}");
}

/// `adopt` removes the lock file of the old path only when no run holds it.
#[test]
fn state_adopt_keeps_an_old_lock_that_a_run_holds() {
    let mut fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    fx.ok(&["apply"]);
    let lock = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "lock"))
        .unwrap();
    let held = fs::File::open(&lock).unwrap();
    held.try_lock().unwrap();
    let old = fx.repo.clone();
    let new = fx.home.join("dotfiles2");
    fs::rename(&old, &new).unwrap();
    (fx.repo, fx.cwd) = (new.clone(), new);
    fx.ok(&["state", "adopt", old.to_str().unwrap()]);
    assert!(lock.exists(), "a held lock stays");
}
