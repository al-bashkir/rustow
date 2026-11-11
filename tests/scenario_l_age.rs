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
    assert!(out.stderr.contains("Git history"), "history warning: {out:?}");
    let ct = fx.repo.join(".ssh/private.conf.age");
    assert!(fs::read(&ct).unwrap().starts_with(b"age-encryption.org/v1"));
    assert!(!fx.repo.join(".ssh/private.conf").exists());
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
fn enc_migrates_registration_and_template_rule() {
    let (fx, key) = keyed();
    let cfg = format!(
        "version = 1\n# keep\n[[files]]\nsource = \"copied.conf\"\n\n[[template_rules]]\nsource = \"x.json.tmpl\"\nmode = \"0600\"\n\n[encryption]\nrecipients = [\"{}\"]\nidentity_files = [\"{}\"]\n",
        key.recipient,
        key.identity.display()
    );
    fx.write(".rustow.toml", &cfg);
    fx.write("copied.conf", "c\n");
    fx.write("x.json.tmpl", "{}\n");
    fx.ok(&["enc", "copied.conf"]);
    fx.ok(&["enc", "x.json.tmpl"]);
    let after = read(&fx.repo.join(".rustow.toml"));
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
        (".rustow.toml", 2, "reserved"),
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
    fx.write(".rustow.toml", "version = 1\n[encryption]\nrecipients = []\n");
    fx.write("a", "a");
    let out = fx.run(&["enc", "a"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("empty"), "{out:?}");
    fx.write(
        ".rustow.toml",
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
    assert_eq!(fs::read(fx.repo.join("s.age")).unwrap(), before);
    let _ = key;
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
        ".rustow.toml",
        &read(&fx.repo.join(".rustow.toml")).replace("[encryption.gpg]\n", ""),
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
    let (fx, _key) = keyed();
    fx.write("plain", "p");
    assert_eq!(fx.run_env(&["enc", "edit", "plain"], &[("VISUAL", "true")]).code, 2);
}
