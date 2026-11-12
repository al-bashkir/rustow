//! Spec §17.2 Scenario M (age) — encrypted deployment.

mod common;

use common::*;
use std::fs;

fn keyed(extra: &str) -> (Fixture, AgeKey) {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[&key], &[&key], extra);
    (fx, key)
}

fn cache_target(dest: &std::path::Path) -> std::path::PathBuf {
    dest.parent().unwrap().join(link_text(dest)).canonicalize().unwrap()
}

#[test]
fn default_apply_decrypts_to_private_cache_and_symlinks() {
    let (fx, key) = keyed("");
    fx.age_encrypt(".ssh/private.conf.age", "Host secret\n", &[&key]);
    fx.ok(&["apply"]);
    let dest = fx.home_path(".ssh/private.conf");
    assert!(is_symlink(&dest));
    assert!(!fx.home_path(".ssh/private.conf.age").exists());
    let cache = cache_target(&dest);
    assert!(cache.starts_with(fx.repo.join(".rustowcache/decrypted")), "{cache:?}");
    assert_eq!(read(&cache), "Host secret\n");
    assert_eq!(mode(&cache), 0o600);
    assert_eq!(
        mode(&fx.home_path(".ssh")),
        0o700,
        "dirs created for secrets are private"
    );
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "idempotent");
}

#[test]
fn registered_ciphertext_deploys_private_regular_file() {
    let (fx, key) = keyed("[[files]]\nsource = \".ssh/copied.conf.age\"\n");
    fx.age_encrypt(".ssh/copied.conf.age", "copied secret\n", &[&key]);
    fx.ok(&["file", "apply"]);
    let dest = fx.home_path(".ssh/copied.conf");
    assert!(is_regular(&dest));
    assert_eq!(read(&dest), "copied secret\n");
    assert_eq!(mode(&dest), 0o600);
    fs::write(&dest, "edited\n").unwrap();
    assert_eq!(fx.run(&["apply"]).code, 3, "independent change is a conflict");
    assert_eq!(read(&dest), "edited\n");
}

#[test]
fn file_add_accepts_ciphertext_and_link_skips_it() {
    let (fx, key) = keyed("");
    fx.age_encrypt("token.age", "t\n", &[&key]);
    fx.ok(&["file", "add", "token.age"]);
    assert!(read(&fx.repo.join(".rustow.toml")).contains("source = \"token.age\""));
    fx.ok(&["link"]);
    assert!(!fx.home_path("token").exists() && !fx.home_path("token.age").exists());
    fx.ok(&["file", "apply"]);
    assert!(is_regular(&fx.home_path("token")));
}

#[test]
fn encrypted_template_decrypts_then_renders_link_or_copy() {
    let (fx, key) =
        keyed("[vars]\nuser = \"me\"\n[[template_rules]]\nsource = \"copy.json.tmpl.age\"\nmode = \"0640\"\n");
    fx.age_encrypt(
        ".config/app/settings.json.tmpl.age",
        "{\"user\": \"{{ vars.user }}\"}\n",
        &[&key],
    );
    fx.age_encrypt("copy.json.tmpl.age", "{{ vars.user }}\n", &[&key]);
    fx.ok(&["template", "apply"]);
    let linked = fx.home_path(".config/app/settings.json");
    assert!(is_symlink(&linked));
    assert_eq!(read(&linked), "{\"user\": \"me\"}\n");
    assert!(cache_target(&linked).starts_with(fx.repo.join(".rustowcache/rendered")));
    assert!(is_regular(&fx.home_path("copy.json")));
    assert_eq!(mode(&fx.home_path("copy.json")), 0o640);
}

#[test]
fn custom_template_extension_with_encryption() {
    let (fx, key) = keyed("[templates]\nextension = \".j2\"\n[vars]\nx = \"y\"\n");
    fx.age_encrypt("a.conf.j2.age", "{{ vars.x }}\n", &[&key]);
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path("a.conf")), "y\n");
}

#[test]
fn plaintext_and_ciphertext_collide_and_nested_suffix_is_skipped() {
    let (fx, key) = keyed("");
    fx.write(".ssh/x", "plain");
    fx.age_encrypt(".ssh/x.age", "secret", &[&key]);
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("same destination"), "{out:?}");
    fs::remove_file(fx.repo.join(".ssh/x")).unwrap();
    fx.write("n.age.gpg", "x");
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr
            .contains("warning: n.age.gpg: nested encryption suffixes are not supported"),
        "{out:?}"
    );
    assert_eq!(read(&fx.home_path(".ssh/x")), "secret", "the rest deploys");
}

#[test]
fn suffix_content_mismatch_fails_without_fallback() {
    let (fx, _key) = keyed("");
    fx.write("fake.age", "this is plaintext, not age\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("not valid age ciphertext"), "{out:?}");
    assert!(!fx.home_path("fake").exists() && !fx.home_path("fake.age").exists());
}

#[test]
fn ignored_ciphertext_needs_no_identity() {
    let fx = Fixture::new();
    fx.write(".rustowignore", "*.age\n");
    fx.write("x.age", "junk");
    fx.write(".bashrc", "b");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn dry_run_writes_no_plaintext_and_reports_missing_keys() {
    let (fx, key) = keyed("");
    fx.age_encrypt("s.age", "secret\n", &[&key]);
    let out = fx.ok(&["apply", "--dry-run"]);
    assert!(out.stdout.contains("~/s"), "{out:?}");
    assert!(!fx.repo.join(".rustowcache").exists());
    let fx2 = Fixture::new();
    fx2.write(".rustow.toml", "version = 1\n");
    fx2.age_encrypt("s.age", "secret\n", &[&key]);
    let out = fx2.run(&["apply", "--dry-run"]);
    assert_eq!(out.code, 4, "an incomplete plan is an error: {out:?}");
}

#[test]
fn failed_decryption_keeps_cache_destination_and_state() {
    let (fx, key) = keyed("");
    fx.age_encrypt("s.age", "v1\n", &[&key]);
    fx.ok(&["apply"]);
    let other = fx.age_key("other");
    fx.age_encrypt("s.age", "v2\n", &[&other]);
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert_eq!(read(&fx.home_path("s")), "v1\n");
}

#[test]
fn cache_tampering_is_detected() {
    let (fx, key) = keyed("");
    fx.age_encrypt("s.age", "v1\n", &[&key]);
    fx.ok(&["apply"]);
    let cache = cache_target(&fx.home_path("s"));
    fs::write(&cache, "tampered\n").unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(read(&cache), "tampered\n");
    assert!(fx.run(&["status"]).stdout.contains("MODIFIED"));
}

#[test]
fn plaintext_to_encrypted_transition_retargets_own_link() {
    let (fx, key) = keyed("");
    fx.write(".ssh/config", "Host plain\n");
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join(".ssh/config")).unwrap();
    fx.age_encrypt(".ssh/config.age", "Host plain\n", &[&key]);
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".ssh/config")), "Host plain\n");
}

#[test]
fn root_scope_encrypted_source_under_temp_root() {
    let (fx, key) = keyed("[[files]]\nsource = \".rustowroot/etc/example/secret.conf.age\"\n");
    fx.age_encrypt(".rustowroot/etc/example/secret.conf.age", "root secret\n", &[&key]);
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.root_path("etc/example/secret.conf")));
    assert_eq!(read(&fx.root_path("etc/example/secret.conf")), "root secret\n");
}

#[test]
fn routine_output_and_state_never_contain_plaintext() {
    let (fx, key) = keyed("[[files]]\nsource = \"c.age\"\n");
    fx.age_encrypt("l.age", "PLAINTEXT-ONE\n", &[&key]);
    fx.age_encrypt("c.age", "PLAINTEXT-TWO\n", &[&key]);
    let mut outputs = vec![
        fx.ok(&["apply", "--json"]),
        fx.run(&["status", "--json", "-v"]),
        fx.run(&["status", "-v"]),
    ];
    outputs.push(fx.ok(&["apply", "-v"]));
    for out in &outputs {
        assert!(
            !out.stdout.contains("PLAINTEXT") && !out.stderr.contains("PLAINTEXT"),
            "{out:?}"
        );
    }
    for entry in fs::read_dir(fx.state.join("rustow")).unwrap().flatten() {
        if entry.path().is_file() {
            assert!(!read(&entry.path()).contains("PLAINTEXT"));
        }
    }
    let diff = fx.run(&["diff"]);
    assert_eq!(diff.code, 0, "{diff:?}");
    fs::write(fx.home_path("c"), "changed\n").unwrap();
    assert!(
        fx.run(&["diff"]).stdout.contains("PLAINTEXT-TWO"),
        "diff is the explicit content-revealing command"
    );
}

#[test]
fn mixed_directory_with_encrypted_member_never_folds() {
    let (fx, key) = keyed("[defaults]\nfold_directories = true\n");
    fx.write(".config/app/plain.conf", "p\n");
    fx.age_encrypt(".config/app/secret.conf.age", "s\n", &[&key]);
    fs::create_dir_all(fx.home_path(".config")).unwrap();
    fx.ok(&["apply"]);
    assert!(fs::symlink_metadata(fx.home_path(".config/app")).unwrap().is_dir());
    assert_eq!(read(&fx.home_path(".config/app/secret.conf")), "s\n");
    assert!(!fx.home_path(".config/app/secret.conf.age").exists());
}
