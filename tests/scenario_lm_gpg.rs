//! Spec §17.2 Scenarios L and M for the GPG backend, run against the
//! installed `gpg` with a disposable GNUPGHOME and agent.

mod common;

use common::*;
use std::fs;

fn setup(extra: &str) -> Option<(Fixture, GpgHome)> {
    if !require_gpg() {
        return None;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("main", false);
    let fpr = home.fpr.clone();
    fx.gpg_config(&home, &[&fpr], extra);
    Some((fx, home))
}

#[test]
fn enc_backend_flag_creates_gpg_that_plain_gpg_decrypts() {
    let Some((fx, home)) = setup("") else { return };
    fx.write("secret.conf", "gpg secret\n");
    fx.ok(&["enc", "--backend", "gpg", "secret.conf"]);
    let ct = fx.repo.join("secret.conf.gpg");
    assert!(!fx.repo.join("secret.conf").exists());
    assert!(!fx.repo.join("secret.conf.age").exists());
    assert_eq!(home.decrypt(&ct), "gpg secret\n");
}

#[test]
fn configured_default_backend_gpg() {
    let Some((fx, home)) = setup("[encryption]\nbackend = \"gpg\"\n") else {
        return;
    };
    // `[encryption]` comes before `[encryption.gpg]` in the generated file.
    fx.write("a", "a\n");
    fx.ok(&["enc", "a"]);
    assert_eq!(home.decrypt(&fx.repo.join("a.gpg")), "a\n");
}

#[test]
fn external_ciphertext_deploys_as_link_copy_and_template() {
    let Some((fx, home)) = setup("[vars]\nname = \"me\"\n[[files]]\nsource = \".ssh/copied.conf.gpg\"\n") else {
        return;
    };
    fx.gpg_encrypt(".ssh/private.conf.gpg", "linked\n", &home);
    fx.gpg_encrypt(".ssh/copied.conf.gpg", "copied\n", &home);
    fx.gpg_encrypt(".config/app.json.tmpl.gpg", "{\"n\": \"{{ vars.name }}\"}\n", &home);
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".ssh/private.conf")));
    assert_eq!(read(&fx.home_path(".ssh/private.conf")), "linked\n");
    assert!(is_regular(&fx.home_path(".ssh/copied.conf")));
    assert_eq!(mode(&fx.home_path(".ssh/copied.conf")), 0o600);
    assert_eq!(read(&fx.home_path(".config/app.json")), "{\"n\": \"me\"}\n");
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "idempotent");
}

#[test]
fn edit_keeps_gpg_backend_when_default_is_age() {
    let Some((fx, home)) = setup("[encryption]\nbackend = \"age\"\n") else {
        return;
    };
    fx.gpg_encrypt("x.gpg", "old\n", &home);
    let editor = fx.editor("ed", "printf 'new\\n' > \"$1\"");
    let out = fx.run_env(&["enc", "edit", "x.gpg"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(home.decrypt(&fx.repo.join("x.gpg")), "new\n");
    assert!(!fx.repo.join("x.age").exists());
    let before = fs::read(fx.repo.join("x.gpg")).unwrap();
    let out = fx.run_env(&["enc", "edit", "x.gpg"], &[("VISUAL", "true")]);
    assert!(out.stdout.contains("unchanged"), "{out:?}");
    assert_eq!(
        fs::read(fx.repo.join("x.gpg")).unwrap(),
        before,
        "no rewrite when nothing changed"
    );
}

#[test]
fn bad_recipients_are_rejected_without_changes() {
    let Some((fx, home)) = setup("") else { return };
    fx.write("p", "plain\n");
    let cases: [(&str, i32, &str); 3] = [
        ("DD87359D", 2, "full key fingerprint"),
        ("user@example.com", 2, "full key fingerprint"),
        ("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", 4, "not in the keyring"),
    ];
    for (recipient, code, word) in cases {
        fx.gpg_config(&home, &[recipient], "");
        let out = fx.run(&["enc", "--backend", "gpg", "p"]);
        assert_eq!(out.code, code, "{recipient}: {out:?}");
        assert!(out.stderr.contains(word), "{recipient}: {out:?}");
    }
    fx.gpg_config(&home, &[], "");
    assert_eq!(fx.run(&["enc", "--backend", "gpg", "p"]).code, 2);
    let stranger = fx.gpg_home("stranger", false);
    home.import_public(&stranger);
    fx.gpg_config(&home, &[&stranger.fpr], "");
    let out = fx.run(&["enc", "--backend", "gpg", "p"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("not trusted"), "{out:?}");
    assert_eq!(read(&fx.repo.join("p")), "plain\n");
    assert!(!fx.repo.join("p.gpg").exists());
}

#[test]
fn age_and_gpg_sources_for_one_destination_collide() {
    let Some((fx, home)) = setup("") else { return };
    fx.gpg_encrypt("x.gpg", "g", &home);
    fx.write("x.age", "a");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("x.age") && out.stdout.contains("x.gpg"), "{out:?}");
}

#[test]
fn locked_key_fails_fast_unattended_then_unlocked_agent_works() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("locked", true);
    let fpr = home.fpr.clone();
    fx.gpg_config(&home, &[&fpr], "");
    let ct = fx.gpg_encrypt("s.gpg", "locked secret\n", &home);
    let started = std::time::Instant::now();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("--interactive"), "{out:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "no hanging prompt"
    );
    assert!(!fx.home_path("s").exists());
    // Unlock the isolated agent once (as a user would), then apply works unattended.
    assert_eq!(home.decrypt(&ct), "locked secret\n");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path("s")), "locked secret\n");
}

#[test]
fn symmetric_truncated_or_mismatched_ciphertext_is_refused() {
    let Some((fx, home)) = setup("") else { return };
    let sym = fx.repo.join("sym.gpg");
    let status = home
        .gpg()
        .args([
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "x",
            "--symmetric",
            "--output",
        ])
        .arg(&sym)
        .arg(fx.write("seed", "s"))
        .status()
        .unwrap();
    assert!(status.success());
    fs::remove_file(fx.repo.join("seed")).unwrap();
    let out = fx.run(&["apply", "sym.gpg"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("public-key"), "{out:?}");

    let full = fs::read(fx.gpg_encrypt("t.gpg", &"y".repeat(5000), &home)).unwrap();
    fs::write(fx.repo.join("t.gpg"), &full[..full.len() / 2]).unwrap();
    assert_eq!(fx.run(&["apply", "t.gpg"]).code, 4);

    fx.write("a.gpg", "age-encryption.org/v1\n-> X25519 abc\n");
    assert_eq!(fx.run(&["apply", "a.gpg"]).code, 4);
    for dest in ["sym", "t", "a"] {
        assert!(fs::symlink_metadata(fx.home_path(dest)).is_err(), "{dest}");
    }
}

#[test]
fn ignored_gpg_and_non_gpg_work_without_gpg_installed() {
    let Some((fx, home)) = setup("") else { return };
    fx.gpg_encrypt("needs.gpg", "n", &home);
    fx.write(".rustowignore", "*.gpg\n");
    fx.write(".bashrc", "b");
    let out = fx.run_env(&["apply"], &[("PATH", "/nonexistent")]);
    assert_eq!(out.code, 0, "{out:?}");
    fs::remove_file(fx.repo.join(".rustowignore")).unwrap();
    let out = fx.run_env(&["apply"], &[("PATH", "/nonexistent")]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("gpg not found"), "{out:?}");
}

#[test]
fn odd_source_names_are_data() {
    let Some((fx, home)) = setup("") else { return };
    fx.gpg_encrypt("-x y.conf.gpg", "odd\n", &home);
    fx.ok(&["apply", "--", "-x y.conf.gpg"]);
    assert_eq!(read(&fx.home_path("-x y.conf")), "odd\n");
}

#[test]
fn dry_run_writes_no_plaintext() {
    let Some((fx, home)) = setup("") else { return };
    fx.gpg_encrypt("s.gpg", "secret\n", &home);
    let out = fx.ok(&["apply", "--dry-run"]);
    assert!(out.stdout.contains("~/s"), "{out:?}");
    assert!(!fx.repo.join(".rustowcache").exists());
    assert!(!out.stdout.contains("secret"));
}

#[test]
fn edit_of_foreign_gpg_ciphertext_with_other_recipient_count_reencrypts() {
    let Some((fx, home)) = setup("") else { return };
    let second = home.add_key("second");
    let first = home.fpr.clone();
    fx.gpg_config(&home, &[&first, &second], "");
    let ct = fx.gpg_encrypt("s.gpg", "x\n", &home);
    let before = fs::read(&ct).unwrap();
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_ne!(fs::read(&ct).unwrap(), before, "re-encrypted to both recipients");
    let listed = home
        .gpg()
        .args(["--list-only", "--status-fd", "1", "--decrypt"])
        .arg(&ct)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&listed.stdout).matches("ENC_TO").count(), 2);
}

/// Audit E4: empty GPG recipients fail before the editor runs, so no
/// plaintext is left in the cache.
#[test]
fn edit_with_empty_gpg_recipients_fails_before_the_editor() {
    let Some((fx, home)) = setup("") else { return };
    let ct = fx.gpg_encrypt("s.gpg", "v1\n", &home);
    let before = fs::read(&ct).unwrap();
    fx.gpg_config(&home, &[], "");
    let marker = fx.base.join("editor-ran");
    let editor = fx.editor("ed", &format!("touch '{}'\nprintf 'v2\\n' > \"$1\"", marker.display()));
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("encryption.gpg.recipients is empty"), "{out:?}");
    assert!(!marker.exists(), "the editor ran");
    assert_eq!(fs::read(&ct).unwrap(), before);
    assert!(fx.workspaces().is_empty(), "plaintext left in the cache");
}

/// Audit E2: `enc edit` may prompt only with --interactive. Unattended,
/// a locked key fails clearly and pinentry never starts.
#[test]
fn enc_edit_with_locked_key_prompts_only_when_interactive() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("locked", true);
    let log = home.stub_pinentry();
    let fpr = home.fpr.clone();
    fx.gpg_config(&home, &[&fpr], "");
    let ct = fx.gpg_encrypt("s.gpg", "v1\n", &home);
    let before = fs::read(&ct).unwrap();
    let editor = fx.editor("ed", "printf 'v2\\n' > \"$1\"");
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("--interactive"), "{out:?}");
    assert!(!log.exists(), "pinentry started unattended");
    assert_eq!(fs::read(&ct).unwrap(), before);
    assert!(fx.workspaces().is_empty());
    let out = fx.run_env(&["--interactive", "enc", "edit", "s.gpg"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(log.exists(), "--interactive allows pinentry");
    assert_eq!(home.decrypt(&ct), "v2\n");
}

/// Audit E5: without state, a swapped GPG recipient with the same count
/// is found from the ENC_TO key ids, and the edit encrypts to the new key.
#[test]
fn edit_without_state_reencrypts_to_a_swapped_gpg_recipient() {
    let Some((fx, home)) = setup("") else { return };
    fx.write("s", "x\n");
    fx.ok(&["enc", "--backend", "gpg", "s"]);
    let ct = fx.repo.join("s.gpg");
    let old = home.enc_to(&ct);
    fs::remove_dir_all(&fx.state).unwrap();
    let second = home.add_key("second");
    fx.gpg_config(&home, &[&second], "");
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", "true")]);
    assert_eq!(out.code, 0, "{out:?}");
    let new = home.enc_to(&ct);
    assert_eq!(new.len(), 1, "{out:?}");
    assert_ne!(new, old, "still encrypted to the old key: {out:?}");
    assert_eq!(home.decrypt(&ct), "x\n");
    // The same recipients without state: nothing to rewrite.
    fs::remove_dir_all(&fx.state).unwrap();
    let before = fs::read(&ct).unwrap();
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", "true")]);
    assert!(out.stdout.contains("unchanged"), "{out:?}");
    assert_eq!(fs::read(&ct).unwrap(), before);
}
