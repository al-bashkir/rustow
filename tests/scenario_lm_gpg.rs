//! Spec §17.2 Scenarios L and M for the GPG backend, run against the
//! installed `gpg` with a disposable GNUPGHOME and agent.

mod common;

use common::*;
use std::fs;
use std::process::Command;

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
    let out = fx.ok(&["enc", "--backend", "gpg", "secret.conf"]);
    assert!(!out.stdout.contains("not verified"), "the secret key verifies: {out:?}");
    let ct = fx.repo.join("secret.conf.gpg");
    assert!(!fx.repo.join("secret.conf").exists());
    assert!(!fx.repo.join("secret.conf.age").exists());
    assert_eq!(home.decrypt(&ct), "gpg secret\n");
}

/// A locked key encrypts, but unattended it cannot verify: the output says
/// that the key is locked, not that it is missing.
#[test]
fn enc_with_a_locked_key_is_not_verified() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("locked", true);
    fx.gpg_config(&home, &[&home.fpr], "");
    fx.write("p", "plain\n");
    let out = fx.ok(&["enc", "--backend", "gpg", "p"]);
    assert!(
        out.stdout
            .contains("not verified: the identity or key is locked; run with --interactive to verify"),
        "{out:?}"
    );
    assert_eq!(home.decrypt(&fx.repo.join("p.gpg")), "plain\n");
}

/// With --interactive, a wrong passphrase leaves the key locked: `enc`
/// keeps the new ciphertext and says that it could not unlock the key.
#[test]
fn enc_with_a_wrong_passphrase_is_not_verified() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("locked", true);
    home.stub_pinentry_answering("wrong");
    fx.gpg_config(&home, &[&home.fpr], "");
    fx.write("p", "plain\n");
    let out = fx.run(&["--interactive", "enc", "--backend", "gpg", "p"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout
            .contains("not verified: the identity or key could not be unlocked"),
        "{out:?}"
    );
    assert_eq!(home.decrypt(&fx.repo.join("p.gpg")), "plain\n");
}

/// An edit that re-encrypts to a key whose secret part is not here says
/// so, as `enc` does: the next edit here cannot decrypt it.
#[test]
fn edit_to_a_recipient_without_a_secret_key_here_is_not_verified() {
    let Some((fx, home)) = setup("") else { return };
    let ct = fx.gpg_encrypt("s.gpg", "v1\n", &home);
    let foreign = home.add_key("foreign");
    let deleted = home
        .gpg()
        .args(["--yes", "--delete-secret-keys", &foreign])
        .output()
        .unwrap();
    assert!(deleted.status.success(), "{}", String::from_utf8_lossy(&deleted.stderr));
    fx.gpg_config(&home, &[&foreign], "");
    let editor = fx.editor("ed", "printf 'v2\\n' > \"$1\"");
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout
            .contains("re-encrypted; deployed files change on the next `rustow apply`; not verified: no matching private key available here"),
        "{out:?}"
    );
    assert_eq!(home.enc_to(&ct).len(), 1);
    assert!(fx.workspaces().is_empty());
}

/// When the new ciphertext does not decrypt here, `enc` fails and keeps
/// the plaintext: it cannot tell a broken file from a missing key.
#[test]
fn enc_fails_when_its_new_ciphertext_does_not_decrypt() {
    let Some((fx, _home)) = setup("") else { return };
    fx.write("p", "plain\n");
    let path = gpg_wrapper(&fx, "*\" --decrypt \"*) echo 'gpg: broken' >&2; exit 2;;");
    let out = fx.run_env(&["enc", "--backend", "gpg", "p"], &[("PATH", &path)]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("new ciphertext failed verification"), "{out:?}");
    assert_eq!(read(&fx.repo.join("p")), "plain\n");
    assert!(!fx.repo.join("p.gpg").exists());
}

/// A PATH whose first `gpg` runs the shell `case` branch `arm` on its
/// arguments, then the real gpg.
fn gpg_wrapper(fx: &Fixture, arm: &str) -> String {
    let real = Command::new("sh")
        .args(["-c", "command -v gpg"])
        .output()
        .unwrap()
        .stdout;
    let real = String::from_utf8(real).unwrap();
    let wrapper = fx.editor(
        "gpg",
        &format!("case \" $* \" in {arm} esac\nexec {} \"$@\"", real.trim()),
    );
    let bin = std::path::Path::new(&wrapper).parent().unwrap().display().to_string();
    format!("{bin}:{}", std::env::var("PATH").unwrap())
}

/// Without encryption.gpg.homedir, $GNUPGHOME selects the keyring.
#[test]
fn gnupghome_selects_the_keyring_without_a_configured_homedir() {
    let Some((fx, home)) = setup("") else { return };
    let config = format!("version = 1\n[encryption.gpg]\nrecipients = [\"{}\"]\n", home.fpr);
    fx.write("rustow.toml", &config);
    fx.write("p", "plain\n");
    let dir = home.dir.display().to_string();
    let out = fx.run_env(&["enc", "--backend", "gpg", "p"], &[("GNUPGHOME", &dir)]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(home.decrypt(&fx.repo.join("p.gpg")), "plain\n");
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

/// A file shared with another person decrypts with our key alone: gpg
/// reports NO_SECKEY for the other recipient and still decrypts.
#[test]
fn ciphertext_for_two_recipients_decrypts_with_one_secret_key() {
    let Some((fx, home)) = setup("") else { return };
    let other = fx.gpg_home("other", false);
    home.import_public(&other);
    fx.gpg_encrypt_to("shared.gpg", "shared\n", &home, &[&home.fpr, &other.fpr]);
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path("shared")), "shared\n");
}

/// A key that can only sign is refused before encryption, with the reason.
#[test]
fn recipient_without_an_encryption_key_is_refused() {
    let Some((fx, home)) = setup("") else { return };
    let out = home
        .gpg()
        .args(["--pinentry-mode", "loopback", "--passphrase", ""])
        .args([
            "--quick-gen-key",
            "sign only <s@example.invalid>",
            "default",
            "sign",
            "never",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let colons = home
        .gpg()
        .args(["--with-colons", "--list-keys", "s@example.invalid"])
        .output()
        .unwrap();
    let colons = String::from_utf8(colons.stdout).unwrap();
    let signer = colons
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .unwrap()
        .split(':')
        .nth(9)
        .unwrap();
    fx.gpg_config(&home, &[signer], "");
    fx.write("p", "plain\n");
    let out = fx.run(&["enc", "--backend", "gpg", "p"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("has no usable encryption subkey"), "{out:?}");
    assert_eq!(read(&fx.repo.join("p")), "plain\n");
    assert!(!fx.repo.join("p.gpg").exists());
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

/// A source that changes while it is encrypted keeps its plaintext, and
/// the moved registration is put back: nothing changes at all.
#[test]
fn source_changed_during_enc_rolls_back_the_registration() {
    let Some((fx, _home)) = setup("") else { return };
    let src = fx.write("p", "plain\n");
    fx.ok(&["file", "add", "p"]);
    let config = read(&fx.repo.join("rustow.toml"));
    let arm = format!("*\" --encrypt \"*) printf 'late\\n' >> '{}';;", src.display());
    let path = gpg_wrapper(&fx, &arm);
    let out = fx.run_env(&["enc", "--backend", "gpg", "p"], &[("PATH", &path)]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains("changed while it was being encrypted"), "{out:?}");
    assert_eq!(
        read(&fx.repo.join("rustow.toml")),
        config,
        "registration not rolled back"
    );
    assert_eq!(read(&src), "plain\nlate\n");
    assert!(!fx.repo.join("p.gpg").exists());
}

/// An untrusted recipient must stop `enc edit` before the editor runs;
/// otherwise encryption fails after the edit and plaintext stays in the cache.
#[test]
fn edit_refuses_an_untrusted_recipient_before_the_editor_runs() {
    let Some((fx, home)) = setup("") else { return };
    let ct = fx.gpg_encrypt("s.gpg", "v1\n", &home);
    let before = fs::read(&ct).unwrap();
    let stranger = fx.gpg_home("stranger", false);
    home.import_public(&stranger);
    fx.gpg_config(&home, &[&home.fpr, &stranger.fpr], "");
    let marker = fx.base.join("editor-ran");
    let editor = fx.editor("ed", &format!("touch '{}'\nprintf 'v2\\n' > \"$1\"", marker.display()));
    let out = fx.run_env(&["enc", "edit", "s.gpg"], &[("VISUAL", &editor)]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("not trusted"), "{out:?}");
    assert!(!marker.exists(), "the editor ran");
    assert_eq!(fs::read(&ct).unwrap(), before);
    assert!(fx.workspaces().is_empty(), "plaintext left in the cache");
}

/// The GPG home must be a directory that others cannot write; rustow
/// refuses it before gpg runs.
#[test]
fn gpg_home_that_is_a_file_or_group_writable_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    fx.write("p", "plain\n");
    let file = write_file(&fx.base.join("g-file"), "");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let open = fx.base.join("g-open");
    fs::create_dir(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o770)).unwrap();
    for dir in [file, open] {
        // Stops any agent that a wrongly accepted home would start.
        let home = GpgHome {
            dir,
            fpr: String::new(),
        };
        fx.gpg_config(&home, &["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"], "");
        let out = fx.run(&["enc", "--backend", "gpg", "p"]);
        assert_eq!(out.code, 4, "{out:?}");
        assert!(out.stderr.contains("must be your own directory"), "{out:?}");
        assert_eq!(read(&fx.repo.join("p")), "plain\n");
    }
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

/// Audit E6: messages say what happened: data that is not OpenPGP is not
/// called symmetric, and a subkey fingerprint is not called missing.
#[test]
fn gpg_messages_name_non_openpgp_data_and_subkey_fingerprints() {
    let Some((fx, home)) = setup("") else { return };
    fx.write("junk.gpg", "this is not OpenPGP\n");
    let out = fx.run(&["apply", "junk.gpg"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("not OpenPGP data"), "{out:?}");
    assert!(!out.stdout.contains("symmetric"), "{out:?}");
    assert!(out.stdout.contains("; gpg: gpg: "), "the first gpg diagnostic: {out:?}");
    assert!(
        !out.stdout.contains("--interactive"),
        "no unlock hint without a key problem: {out:?}"
    );
    fs::remove_file(fx.repo.join("junk.gpg")).unwrap();
    let colons = home
        .gpg()
        .args(["--with-colons", "--list-keys", &home.fpr])
        .output()
        .unwrap()
        .stdout;
    let colons = String::from_utf8(colons).unwrap();
    let subkey = colons
        .lines()
        .skip_while(|l| !l.starts_with("sub:"))
        .find(|l| l.starts_with("fpr:"))
        .unwrap()
        .split(':')
        .nth(9)
        .unwrap()
        .to_owned();
    fx.gpg_config(&home, &[&subkey], "");
    fx.write("p", "plain\n");
    let out = fx.run(&["enc", "--backend", "gpg", "p"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(
        out.stderr.contains("subkey") && out.stderr.contains(&home.fpr),
        "{out:?}"
    );
    assert!(!out.stderr.contains("not in the keyring"), "{out:?}");
    assert_eq!(read(&fx.repo.join("p")), "plain\n");
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

/// Spec Scenario L: explicit interactive mode with a controlled pinentry.
/// A locked key fails without --interactive and works with it.
#[test]
fn locked_key_apply_uses_pinentry_only_with_interactive() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("locked", true);
    let log = home.stub_pinentry();
    let fpr = home.fpr.clone();
    fx.gpg_config(&home, &[&fpr], "");
    fx.gpg_encrypt("s.gpg", "locked secret\n", &home);
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("--interactive"), "{out:?}");
    assert!(!log.exists(), "pinentry started without --interactive");
    assert!(!fx.home_path("s").exists());
    let out = fx.run(&["--interactive", "apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(log.exists(), "pinentry was not asked");
    assert_eq!(read(&fx.home_path("s")), "locked secret\n");
}
