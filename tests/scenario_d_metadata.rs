//! Spec §17.2 Scenario D — templates with explicit metadata are copied.

mod common;

use common::*;

fn with_rule(rule: &str) -> Fixture {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        &format!(
            "version = 1\n[vars]\nhost = \"github.com\"\n[[template_rules]]\nsource = \".ssh/config.tmpl\"\n{rule}\n"
        ),
    );
    fx.write(".ssh/config.tmpl", "Host {{ vars.host }}\n");
    fx
}

#[test]
fn scenario_d_full_flow() {
    let fx = with_rule("mode = \"0600\"");
    fx.ok(&["template", "apply"]);
    let dest = fx.home_path(".ssh/config");
    assert!(is_regular(&dest), "metadata selects copy deployment");
    assert_eq!(read(&dest), "Host github.com\n");
    assert_eq!(mode(&dest), 0o600);
    // A metadata-only change is applied without rewriting content.
    fx.write(
        ".rustow.toml",
        "version = 1\n[vars]\nhost = \"github.com\"\n[[template_rules]]\nsource = \".ssh/config.tmpl\"\nmode = \"0640\"\n",
    );
    let diff = fx.run(&["diff"]);
    assert!(
        diff.stdout.contains("mode 0600") && diff.stdout.contains("mode 0640"),
        "{diff:?}"
    );
    let out = fx.ok(&["template", "apply"]);
    assert!(out.stdout.contains("metadata"), "{out:?}");
    assert_eq!(mode(&dest), 0o640);
    assert!(is_regular(&dest));
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
}

#[test]
fn owner_and_group_by_name_and_id_resolve() {
    let uid = nix::unistd::geteuid().as_raw();
    let gid = nix::unistd::getegid().as_raw();
    let group = nix::unistd::Group::from_gid(nix::unistd::getegid())
        .unwrap()
        .unwrap()
        .name;
    let fx = with_rule(&format!("owner = \"{uid}\"\ngroup = \"{group}\""));
    fx.ok(&["apply"]);
    let meta = std::fs::metadata(fx.home_path(".ssh/config")).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!((meta.uid(), meta.gid()), (uid, gid));
}

#[test]
fn unknown_owner_is_an_explicit_error() {
    let fx = with_rule("owner = \"no-such-user-rustow\"");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("no-such-user-rustow"), "{out:?}");
    assert!(!fx.home_path(".ssh/config").exists());
}

#[test]
fn chown_to_root_without_privilege_fails_clearly() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: running as root");
        return;
    }
    let fx = with_rule("owner = \"0\"");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("set owner"), "{out:?}");
    assert!(!fx.home_path(".ssh/config").exists(), "no partial file is left");
}

#[test]
fn copy_to_link_transition_needs_authorization() {
    let fx = with_rule("mode = \"0600\"");
    fx.ok(&["apply"]);
    fx.write(".rustow.toml", "version = 1\n[vars]\nhost = \"github.com\"\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    fx.ok(&["apply", "--force"]);
    assert!(is_symlink(&fx.home_path(".ssh/config")));
}

#[test]
fn link_to_copy_transition_is_automatic() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n");
    fx.write(".ssh/config.tmpl", "Host x\n");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".ssh/config")));
    fx.write(
        ".rustow.toml",
        "version = 1\n[[template_rules]]\nsource = \".ssh/config.tmpl\"\nmode = \"0600\"\n",
    );
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.home_path(".ssh/config")));
    assert_eq!(mode(&fx.home_path(".ssh/config")), 0o600);
}
