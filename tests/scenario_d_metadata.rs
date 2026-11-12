//! Spec §17.2 Scenario D — templates with explicit metadata are copied.

mod common;

use common::*;

fn with_rule(rule: &str) -> Fixture {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
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
        "rustow.toml",
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

/// `check` finds the unknown names that `apply` would fail on.
#[test]
fn check_reports_unknown_owner_and_group_names() {
    let fx = with_rule("owner = \"nosuchuser\"\ngroup = \"nosuchgroup\"");
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 1, "{out:?}");
    for want in ["unknown user 'nosuchuser'", "unknown group 'nosuchgroup'"] {
        assert!(
            out.stdout
                .lines()
                .any(|l| l.starts_with("problem: ") && l.contains(".ssh/config.tmpl") && l.contains(want)),
            "{want}: {out:?}"
        );
    }
    assert!(out.stdout.contains("2 problems"), "{out:?}");
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

/// A change of the owner alone on a deployed copy still runs chown:
/// without privilege, owner 0 fails clearly and the content stays.
#[test]
fn owner_only_change_without_privilege_fails_clearly() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: running as root");
        return;
    }
    let fx = with_rule("mode = \"0600\"");
    fx.ok(&["apply"]);
    fx.write(
        "rustow.toml",
        "version = 1\n[vars]\nhost = \"github.com\"\n[[template_rules]]\nsource = \".ssh/config.tmpl\"\nmode = \"0600\"\nowner = \"0\"\n",
    );
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("set owner"), "{out:?}");
    assert_eq!(read(&fx.home_path(".ssh/config")), "Host github.com\n");
}

#[test]
fn copy_to_link_transition_needs_authorization() {
    let fx = with_rule("mode = \"0600\"");
    fx.ok(&["apply"]);
    fx.write("rustow.toml", "version = 1\n[vars]\nhost = \"github.com\"\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    fx.ok(&["apply", "--force"]);
    assert!(is_symlink(&fx.home_path(".ssh/config")));
}

#[test]
fn mode_without_owner_read_is_a_configuration_error() {
    for bad in ["0200", "044", "+64"] {
        let fx = with_rule(&format!("mode = \"{bad}\""));
        let out = fx.run(&["check"]);
        assert_eq!(out.code, 2, "{bad}: {out:?}");
        assert!(out.stderr.contains("mode"), "{bad}: {out:?}");
        let out = fx.run(&["apply"]);
        assert_eq!(out.code, 2, "{bad}: {out:?}");
        assert!(!fx.home_path(".ssh/config").exists(), "{bad}: nothing deployed");
    }
}

#[test]
fn enc_keeps_comments_of_a_migrated_template_rule() {
    let fx = Fixture::new();
    let key = fx.age_key("me");
    let rule = "[[template_rules]]\n# ssh needs 0600\nsource = \".ssh/config.tmpl\" # the main one\nmode = \"0600\"\n";
    fx.age_config(&[&key], &[&key], rule);
    fx.write(".ssh/config.tmpl", "Host x\n");
    fx.ok(&["enc", ".ssh/config.tmpl"]);
    let after = read(&fx.repo.join("rustow.toml"));
    assert!(
        after.contains("# ssh needs 0600\nsource = \".ssh/config.tmpl.age\" # the main one\n"),
        "{after}"
    );
    fx.ok(&["apply"]);
    assert_eq!(mode(&fx.home_path(".ssh/config")), 0o600);
}

#[test]
fn file_add_and_remove_keep_crlf_line_endings() {
    let fx = Fixture::new();
    let config = "version = 1\r\n# mine\r\n[vars]\r\na = 1\r\n[[files]]\r\nsource = \"a.conf\"\r\n\r\n[[files]]\r\nsource = \"b.conf\"\r\n";
    fx.write("rustow.toml", config);
    fx.write("a.conf", "a");
    fx.write("b.conf", "b");
    fx.write("c.conf", "c");
    fx.ok(&["file", "add", "c.conf"]);
    assert_eq!(
        read(&fx.repo.join("rustow.toml")),
        config,
        "file add writes the state only"
    );
    fx.ok(&["file", "remove", "a.conf"]);
    let removed = read(&fx.repo.join("rustow.toml"));
    assert!(!removed.contains("a.conf") && removed.contains("b.conf"), "{removed:?}");
    assert_eq!(
        removed.matches('\n').count(),
        removed.matches("\r\n").count(),
        "{removed:?}"
    );
}

#[test]
fn link_to_copy_transition_is_automatic() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(".ssh/config.tmpl", "Host x\n");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".ssh/config")));
    fx.write(
        "rustow.toml",
        "version = 1\n[[template_rules]]\nsource = \".ssh/config.tmpl\"\nmode = \"0600\"\n",
    );
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.home_path(".ssh/config")));
    assert_eq!(mode(&fx.home_path(".ssh/config")), 0o600);
}
