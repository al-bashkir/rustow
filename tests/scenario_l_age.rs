//! Spec §17.2 Scenario L (age) — `rustow enc` and protected `enc edit`.

mod common;

use common::*;
use std::fs;

fn keyed() -> (Fixture, AgeKey) {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[&key], &[&key], "");
    (fx, key)
}

#[test]
fn enc_defaults_to_age_and_retires_plaintext() {
    let (fx, key) = keyed();
    fx.write(".ssh/private.conf", "Host secret\n");
    let out = fx.ok(&["enc", ".ssh/private.conf"]);
    assert!(out.stdout.contains(".ssh/private.conf.age"), "{out:?}");
    assert!(!out.stdout.contains("not verified"), "the identity verifies: {out:?}");
    assert!(out.stdout.lines().any(|l| l == "1 changed, 0 unchanged"), "{out:?}");
    assert!(out.stderr.contains("Git history"), "history warning: {out:?}");
    let ct = fx.repo.join(".ssh/private.conf.age");
    assert!(fs::read(&ct).unwrap().starts_with(b"age-encryption.org/v1"));
    assert!(!fx.repo.join(".ssh/private.conf").exists());
    let names: Vec<_> = fs::read_dir(fx.repo.join(".ssh"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["private.conf.age"], "no plaintext copy is left behind");
    assert_eq!(fx.age_decrypt(&ct, &key), "Host secret\n");
    assert!(!fx.home_path(".ssh").exists(), "enc deploys nothing");
}

#[test]
fn enc_with_public_recipients_only_and_decrypt_needs_identity() {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[&key], &[], "");
    fx.write("token", "t0k3n\n");
    let out = fx.ok(&["enc", "token"]);
    assert!(out.stdout.contains("not verified"), "{out:?}");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("no age identities configured"), "{out:?}");
    assert!(!out.stdout.contains("t0k3n"));
}

#[test]
fn enc_with_only_a_non_matching_identity_is_not_verified() {
    let fx = Fixture::new();
    let (key, other) = (fx.age_key("main"), fx.age_key("other"));
    fx.age_config(&[&key], &[&other], "");
    fx.write("token", "t0k3n\n");
    let out = fx.ok(&["enc", "token"]);
    assert!(out.stdout.contains("not verified"), "{out:?}");
    assert_eq!(fx.age_decrypt(&fx.repo.join("token.age"), &key), "t0k3n\n");
}

#[test]
fn enc_migrates_registration_and_template_rule() {
    let (fx, key) = keyed();
    let cfg = format!(
        "version = 1\n# keep\n[[files]]\nsource = \"copied.conf\"\n\n[[template_rules]]\nsource = \"x.json.tmpl\"\nmode = \"0600\"\n\n[encryption]\nrecipients = [\"{}\"]\nidentity_files = [\"{}\"]\n",
        key.recipient,
        key.identity.display()
    );
    fx.write("rustow.toml", &cfg);
    fx.write("copied.conf", "c\n");
    fx.write("x.json.tmpl", "{}\n");
    fx.ok(&["enc", "copied.conf"]);
    fx.ok(&["enc", "x.json.tmpl"]);
    let after = read(&fx.repo.join("rustow.toml"));
    assert!(
        after.contains("source = \"copied.conf.age\"") && after.contains("source = \"x.json.tmpl.age\""),
        "{after}"
    );
    assert!(after.contains("mode = \"0600\"") && after.contains("# keep"), "{after}");
    assert_eq!(after.matches("[[files]]").count(), 1);
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.home_path("copied.conf")));
    assert_eq!(mode(&fx.home_path("x.json")), 0o600);
}

#[test]
fn enc_moves_state_registrations_to_the_encrypted_name() {
    let (fx, _key) = keyed();
    fx.write(
        "rustow.toml",
        &read(&fx.repo.join("rustow.toml")).replace("version = 1\n", "version = 1\n[defaults]\nmode = \"copy\"\n"),
    );
    fx.write("copied.conf", "c\n");
    fx.write("linked.conf", "l\n");
    fx.ok(&["file", "add", "copied.conf"]);
    fx.ok(&["link", "linked.conf"]);
    for source in ["copied.conf", "linked.conf"] {
        let out = fx.ok(&["enc", source]);
        assert!(out.stdout.contains("registrations moved"), "{out:?}");
    }
    let state = fx.state_json();
    assert_eq!(state["files"], serde_json::json!(["copied.conf.age"]));
    assert_eq!(state["links"], serde_json::json!(["linked.conf.age"]));
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.home_path("copied.conf")));
    assert!(is_symlink(&fx.home_path("linked.conf")), "decrypt+link");
}

#[test]
fn enc_rejects_bad_inputs_without_changes() {
    let (fx, _key) = keyed();
    fx.write("a.conf", "a");
    fx.write("a.conf.age", "existing");
    fx.write("b.age", "already");
    fx.write("real", "r");
    std::os::unix::fs::symlink("real", fx.repo.join("link")).unwrap();
    for (path, code, word) in [
        ("a.conf", 3, "already exists"),
        ("b.age", 2, "already encrypted"),
        ("link", 2, "symlink"),
        ("rustow.toml", 2, "reserved"),
        ("missing", 2, "does not exist"),
    ] {
        let out = fx.run(&["enc", path]);
        assert_eq!(out.code, code, "{path}: {out:?}");
        assert!(out.stderr.contains(word), "{path}: {out:?}");
    }
    assert_eq!(read(&fx.repo.join("a.conf")), "a");
    assert_eq!(read(&fx.repo.join("a.conf.age")), "existing");
}

#[test]
fn enc_rejects_empty_or_invalid_recipients() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[encryption]\nrecipients = []\n");
    fx.write("a", "a");
    let out = fx.run(&["enc", "a"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("empty"), "{out:?}");
    fx.write(
        "rustow.toml",
        "version = 1\n[encryption]\nrecipients = [\"age1bogus\"]\n",
    );
    assert_eq!(fx.run(&["enc", "a"]).code, 2);
    assert_eq!(read(&fx.repo.join("a")), "a");
    assert!(!fx.repo.join("a.age").exists());
}

#[test]
fn edit_persists_ciphertext_and_leaves_destination_until_apply() {
    let (fx, key) = keyed();
    fx.age_encrypt(".ssh/private.conf.age", "v1\n", &[&key]);
    fx.ok(&["apply"]);
    let perms = fx.base.join("perms.txt");
    let editor = fx.editor(
        "ed",
        &format!(
            "ls -ld \"$1\" \"$(dirname \"$1\")\" > {}\nprintf 'v2\\n' > \"$1\"",
            perms.display()
        ),
    );
    let out = fx.run_env(&["enc", "edit", ".ssh/private.conf.age"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(fx.age_decrypt(&fx.repo.join(".ssh/private.conf.age"), &key), "v2\n");
    assert_eq!(
        read(&fx.home_path(".ssh/private.conf")),
        "v1\n",
        "deployed copy changes only on apply"
    );
    let listing = read(&perms);
    assert!(
        listing.contains("-rw-------") && listing.contains("drwx------"),
        "{listing}"
    );
    assert!(
        listing.contains("private.conf\n") || listing.contains("private.conf "),
        "logical name: {listing}"
    );
    assert!(fx.workspaces().is_empty(), "workspace removed");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".ssh/private.conf")), "v2\n");
}

#[test]
fn edit_unchanged_does_not_rewrite() {
    let (fx, key) = keyed();
    fx.write("s", "same\n");
    fx.ok(&["enc", "s"]);
    let before = fs::read(fx.repo.join("s.age")).unwrap();
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains("unchanged"), "{out:?}");
    assert!(out.stdout.lines().any(|l| l == "0 changed, 1 unchanged"), "{out:?}");
    assert_eq!(fs::read(fx.repo.join("s.age")).unwrap(), before);
    let _ = key;
}

/// The state record of the ciphertext shows a swapped recipient that the
/// age format hides (the recipient count stays one).
#[test]
fn edit_with_state_reencrypts_to_a_swapped_recipient() {
    let (fx, key) = keyed();
    fx.write("s", "x\n");
    fx.ok(&["enc", "s"]);
    let second = fx.age_key("second");
    fx.age_config(&[&second], &[&key, &second], "");
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(fx.age_decrypt(&fx.repo.join("s.age"), &second), "x\n");
}

#[test]
fn edit_with_changed_recipients_reencrypts_same_backend() {
    let (fx, key) = keyed();
    fx.write("s", "same\n");
    fx.ok(&["enc", "s"]);
    let before = fs::read(fx.repo.join("s.age")).unwrap();
    let second = fx.age_key("second");
    fx.age_config(&[&key, &second], &[&key], "[encryption.gpg]\n");
    fx.write(
        "rustow.toml",
        &read(&fx.repo.join("rustow.toml")).replace("[encryption.gpg]\n", ""),
    );
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_ne!(fs::read(fx.repo.join("s.age")).unwrap(), before);
    assert_eq!(fx.age_decrypt(&fx.repo.join("s.age"), &second), "same\n");
}

#[test]
fn reencrypt_flag_forces_rewrite() {
    let (fx, key) = keyed();
    fx.age_encrypt("s.age", "x\n", &[&key]);
    let before = fs::read(fx.repo.join("s.age")).unwrap();
    let out = fx.run_env(&["enc", "edit", "--reencrypt", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.lines().any(|l| l == "1 changed, 0 unchanged"), "{out:?}");
    assert_ne!(fs::read(fx.repo.join("s.age")).unwrap(), before);
    assert_eq!(fx.age_decrypt(&fx.repo.join("s.age"), &key), "x\n");
}

#[test]
fn editor_failure_keeps_ciphertext_and_cleans_up() {
    let (fx, key) = keyed();
    fx.age_encrypt("s.age", "keep\n", &[&key]);
    let before = fs::read(fx.repo.join("s.age")).unwrap();
    let editor = fx.editor("fail", "printf 'lost\\n' > \"$1\"\nexit 3");
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 4, "{out:?}");
    assert_eq!(fs::read(fx.repo.join("s.age")).unwrap(), before);
    assert!(fx.workspaces().is_empty());
}

#[test]
fn concurrent_ciphertext_change_is_a_conflict_and_keeps_the_edit() {
    let (fx, key) = keyed();
    let ct = fx.age_encrypt("s.age", "base\n", &[&key]);
    let editor = fx.editor(
        "race",
        &format!("printf 'other' > {}\nprintf 'mine\\n' > \"$1\"", ct.display()),
    );
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(read(&ct), "other");
    let kept = fx.workspaces();
    assert_eq!(kept.len(), 1, "{out:?}");
    assert!(out.stderr.contains(kept[0].to_str().unwrap()), "{out:?}");
    // The kept workspace is stale once no edit holds it.
    let clean = fx.ok(&["cache", "clean"]);
    assert!(clean.stdout.contains("edit"), "{clean:?}");
    assert!(fx.workspaces().is_empty());
}

#[test]
fn wrong_identity_or_corrupt_ciphertext_preserves_everything() {
    let (fx, key) = keyed();
    let other = fx.age_key("other");
    fx.age_encrypt("s.age", "secret\n", &[&other]);
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("no configured age identity"), "{out:?}");
    fs::write(fx.repo.join("t.age"), b"age-encryption.org/v1\n-> X25519 trunc").unwrap();
    let out = fx.run_env(&["enc", "edit", "t.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("not valid age ciphertext"), "{out:?}");
    assert!(fx.workspaces().is_empty());
    let _ = key;
}

#[test]
fn enc_edit_rejects_plaintext_and_nested_suffix() {
    let (fx, key) = keyed();
    fx.write("plain", "p");
    assert_eq!(fx.run_env(&["enc", "edit", "plain"], &[("VISUAL", "true")]).code, 2);
    // Audit E3: apply rejects nested suffixes, so enc edit must too.
    fx.age_encrypt("wrap.gpg.age", "w\n", &[&key]);
    fx.write("wrap.age.gpg", "x");
    for path in ["wrap.gpg.age", "wrap.age.gpg"] {
        let out = fx.run_env(&["enc", "edit", path], &[("VISUAL", "true")]);
        assert_eq!(out.code, 2, "{path}: {out:?}");
        assert!(out.stderr.contains("nested"), "{path}: {out:?}");
    }
    assert!(fx.workspaces().is_empty());
}

#[test]
fn doctor_checks_identity_files() {
    use std::os::unix::fs::PermissionsExt;
    let (fx, key) = keyed();
    let ok = fx.run(&["doctor"]);
    assert!(ok.stdout.contains("age identity"), "{ok:?}");
    fs::set_permissions(&key.identity, fs::Permissions::from_mode(0o644)).unwrap();
    let out = fx.run(&["doctor"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("chmod 600"), "{out:?}");
}

#[test]
fn editor_failure_message_names_the_editor_status() {
    let (fx, key) = keyed();
    fx.age_encrypt("s.age", "keep\n", &[&key]);
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "false")]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("editor exited with"), "{out:?}");
}

#[test]
fn signal_during_edit_is_handled_and_cleaned_up() {
    let (fx, key) = keyed();
    let ct = fx.age_encrypt("s.age", "keep\n", &[&key]);
    let before = fs::read(&ct).unwrap();
    // The editor asks rustow (its parent) to stop, then fails.
    let editor = fx.editor("sig", "kill -TERM $PPID\nsleep 0.2\nexit 1");
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("interrupted"), "{out:?}");
    assert_eq!(fs::read(&ct).unwrap(), before);
    assert!(fx.workspaces().is_empty());
}

#[test]
fn edit_of_foreign_ciphertext_with_other_recipient_count_reencrypts() {
    let (fx, key) = keyed();
    let second = fx.age_key("second");
    fx.age_config(&[&key, &second], &[&key], "");
    // Made outside rustow, so no state record says who it was for.
    let ct = fx.age_encrypt("s.age", "x\n", &[&key]);
    let before = fs::read(&ct).unwrap();
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_ne!(fs::read(&ct).unwrap(), before);
    assert_eq!(fx.age_decrypt(&ct, &second), "x\n");
    // Same count as configured: left alone.
    let ct2 = fx.age_encrypt("t.age", "y\n", &[&key, &second]);
    let before2 = fs::read(&ct2).unwrap();
    assert_eq!(fx.run_env(&["enc", "edit", "t.age"], &[("VISUAL", "true")]).code, 0);
    assert_eq!(fs::read(&ct2).unwrap(), before2);
}

/// Audit E4: invalid recipients fail before decryption and before the
/// editor runs, so no plaintext is left in the cache.
#[test]
fn edit_checks_recipients_before_the_editor_runs() {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[], &[&key], "");
    let ct = fx.age_encrypt("s.age", "v1\n", &[&key]);
    let before = fs::read(&ct).unwrap();
    let marker = fx.base.join("editor-ran");
    let editor = fx.editor("ed", &format!("touch '{}'\nprintf 'v2\\n' > \"$1\"", marker.display()));
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("encryption.recipients is empty"), "{out:?}");
    assert!(!marker.exists(), "the editor ran");
    assert_eq!(fs::read(&ct).unwrap(), before);
    assert!(fx.workspaces().is_empty(), "plaintext left in the cache");
}

/// Audit E1: a symlinked parent directory must not lead `enc` or
/// `enc edit` to files outside the repository.
#[test]
fn enc_and_edit_refuse_a_symlinked_parent_directory() {
    let (fx, key) = keyed();
    let outside = fx.base.join("outside");
    let plain = write_file(&outside.join("o.txt"), "outside\n");
    std::os::unix::fs::symlink(&outside, fx.repo.join("ld")).unwrap();
    let refused = "source 'ld' is a symbolic link; rustow encrypts only files inside the repository; use the real path";
    let out = fx.run(&["enc", "ld/o.txt"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains(refused), "{out:?}");
    assert!(!out.stderr.contains(".rustowignore"), "{out:?}");
    assert_eq!(read(&plain), "outside\n");
    assert!(!outside.join("o.txt.age").exists());
    let ct = fx.age_encrypt("ld/p.age", "v1\n", &[&key]);
    let before = fs::read(&ct).unwrap();
    let editor = fx.editor("ed", "printf 'v2\\n' > \"$1\"");
    let out = fx.run_env(&["enc", "edit", "ld/p.age"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains(refused), "{out:?}");
    assert!(!out.stderr.contains(".rustowignore"), "{out:?}");
    assert_eq!(fs::read(&ct).unwrap(), before);
    assert!(fx.workspaces().is_empty());
    // apply skips the symlink with its own hint.
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr.contains(
            "warning: ld: is a symbolic link; rustow deploys regular files only. Add it to .rustowignore to skip it"
        ),
        "{out:?}"
    );
}

/// The edit workspace holds plaintext: a symlink in its place is refused.
#[test]
fn edit_refuses_a_symlinked_workspace_root() {
    let (fx, _key) = keyed();
    fx.write("s", "secret\n");
    fx.ok(&["enc", "s"]);
    let victim = fx.home_path("victim");
    fs::create_dir_all(&victim).unwrap();
    let edit = fx.repo.join(".rustowcache/edit");
    let _ = fs::remove_dir_all(&edit);
    fs::create_dir_all(fx.repo.join(".rustowcache")).unwrap();
    std::os::unix::fs::symlink(&victim, &edit).unwrap();
    let out = fx.run_env(&["enc", "edit", "s.age"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("symlinks are not followed"), "{out:?}");
    assert_eq!(
        fs::read_dir(&victim).unwrap().count(),
        0,
        "no plaintext outside the cache"
    );
}

/// The editor gets plaintext: an editor named without a `/` is looked up
/// only in absolute PATH entries, never in the current directory.
#[test]
fn edit_never_runs_an_editor_from_the_current_directory() {
    let (mut fx, key) = keyed();
    fx.write("s", "x\n");
    fx.ok(&["enc", "s"]);
    let marker = fx.base.join("ran");
    fx.editor("planted", &format!("touch '{}'", marker.display()));
    fx.cwd = fx.base.join("bin");
    let out = fx.run_env(
        &["enc", "edit", "s.age"],
        &[("VISUAL", "planted"), ("PATH", ":/usr/bin:/bin")],
    );
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("cannot run editor 'planted'"), "{out:?}");
    assert!(!marker.exists(), "{out:?}");
    assert_eq!(fx.age_decrypt(&fx.repo.join("s.age"), &key), "x\n");
}

/// `enc` changes the repository and the state, so it takes the repository
/// lock: while another run holds it, `enc` changes nothing.
#[test]
fn enc_stops_while_another_run_holds_the_lock() {
    let (fx, _key) = keyed();
    fx.write("s", "x\n");
    fx.ok(&["apply"]);
    let lock = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "lock"))
        .unwrap();
    let held = fs::OpenOptions::new().write(true).open(&lock).unwrap();
    held.try_lock().unwrap();
    let out = fx.run(&["enc", "s"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("another rustow process"), "{out:?}");
    assert!(!fx.repo.join("s.age").exists() && fx.repo.join("s").exists());
}
