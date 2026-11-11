//! Spec §17.2 Scenario G — one unified apply over every mode and both backends.

mod common;

use common::*;

#[test]
fn unified_plan_apply_and_idempotency() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let age = fx.age_key("age");
    let gpg = fx.gpg_home("gpg", false);
    let config = format!(
        "version = 1\n[vars]\nname = \"Example User\"\n\
         [[template_rules]]\nsource = \".rustowroot/etc/example/config.conf.tmpl\"\nmode = \"0640\"\n\
         [encryption]\nrecipients = [\"{}\"]\nidentity_files = [\"{}\"]\n\
         [encryption.gpg]\nrecipients = [\"{}\"]\nhomedir = \"{}\"\n",
        age.recipient,
        age.identity.display(),
        gpg.fpr,
        gpg.dir.display()
    );
    fx.write(".rustow.toml", &config);
    fx.write(".bashrc", "bash\n");
    fx.write(".gitconfig.tmpl", "[user]\n    name = {{ vars.name }}\n");
    fx.age_encrypt(".ssh/private.conf.age", "age linked\n", &[&age]);
    fx.age_encrypt(".ssh/copied-secret.conf.age", "age copied\n", &[&age]);
    fx.gpg_encrypt(".ssh/gpg-private.conf.gpg", "gpg linked\n", &gpg);
    fx.gpg_encrypt(".ssh/gpg-copied-secret.conf.gpg", "gpg copied\n", &gpg);
    fx.write(".config/example/settings.json", "{}\n");
    fx.gpg_encrypt(
        ".config/example/gpg-settings.json.tmpl.gpg",
        "{\"n\": \"{{ vars.name }}\"}\n",
        &gpg,
    );
    fx.write(".rustowroot/etc/example/config.conf.tmpl", "user={{ vars.name }}\n");
    for f in [
        ".config/example/settings.json",
        ".ssh/copied-secret.conf.age",
        ".ssh/gpg-copied-secret.conf.gpg",
    ] {
        fx.ok(&["file", "add", f]);
    }

    let plan = fx.ok(&["apply", "--dry-run", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&plan.stdout).unwrap();
    let items = v["items"].as_array().unwrap();
    let mode_of = |source: &str| {
        let item = items
            .iter()
            .find(|i| i["source"] == source)
            .unwrap_or_else(|| panic!("{source} not planned"));
        assert_eq!(item["action"], "create", "{item}");
        item["mode"].as_str().unwrap().to_owned()
    };
    assert_eq!(mode_of(".bashrc"), "link");
    assert_eq!(mode_of(".gitconfig.tmpl"), "template+link");
    assert_eq!(mode_of(".config/example/settings.json"), "copy");
    assert_eq!(mode_of(".rustowroot/etc/example/config.conf.tmpl"), "template+copy");
    assert_eq!(mode_of(".ssh/private.conf.age"), "decrypt+link");
    assert_eq!(mode_of(".ssh/copied-secret.conf.age"), "decrypt+copy");
    assert_eq!(mode_of(".ssh/gpg-private.conf.gpg"), "decrypt+link");
    assert_eq!(mode_of(".ssh/gpg-copied-secret.conf.gpg"), "decrypt+copy");
    assert_eq!(mode_of(".config/example/gpg-settings.json.tmpl.gpg"), "template+link");
    assert_eq!(items.len(), 9);
    assert!(!fx.repo.join(".rustowcache").exists(), "dry run writes nothing");

    fx.ok(&["apply"]);
    let h = |p: &str| fx.home_path(p);
    assert!(is_symlink(&h(".bashrc")));
    assert!(is_symlink(&h(".gitconfig")) && read(&h(".gitconfig")).contains("Example User"));
    assert!(is_regular(&h(".config/example/settings.json")));
    assert!(is_symlink(&h(".ssh/private.conf")) && read(&h(".ssh/private.conf")) == "age linked\n");
    assert!(is_regular(&h(".ssh/copied-secret.conf")) && mode(&h(".ssh/copied-secret.conf")) == 0o600);
    assert!(is_symlink(&h(".ssh/gpg-private.conf")) && read(&h(".ssh/gpg-private.conf")) == "gpg linked\n");
    assert!(is_regular(&h(".ssh/gpg-copied-secret.conf")) && read(&h(".ssh/gpg-copied-secret.conf")) == "gpg copied\n");
    assert_eq!(
        read(&h(".config/example/gpg-settings.json")),
        "{\"n\": \"Example User\"}\n"
    );
    let root_conf = fx.root_path("etc/example/config.conf");
    assert!(is_regular(&root_conf) && mode(&root_conf) == 0o640);
    assert_eq!(read(&root_conf), "user=Example User\n");

    let again = fx.ok(&["apply"]);
    assert!(again.stdout.contains("0 changed, 9 unchanged"), "{again:?}");
    assert_eq!(fx.run(&["status"]).code, 0);
}
