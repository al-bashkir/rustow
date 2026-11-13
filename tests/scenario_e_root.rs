//! Spec §17.2 Scenario E — `rustowroot/` mapped to a temporary root target.
//! System files deploy only as copies, never as symlinks.

mod common;

use common::*;
use std::fs;

/// The reason an unregistered system file is skipped: the `file add` hint.
fn reason(source: &str) -> String {
    format!("system files deploy only as copies; register it with `rustow file add {source}`")
}

/// The error when a command names an unregistered system file.
fn hint(source: &str) -> String {
    format!("source '{source}': {}", reason(source))
}

/// The warning of a deploy command for an unregistered system file.
fn warning(source: &str) -> String {
    format!("warning: {source}: {}", reason(source))
}

#[test]
fn unregistered_root_file_is_skipped_until_added() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    let source = "rustowroot/etc/example/config.conf";
    fx.write(source, "plain\n");
    let out = fx.ok(&["apply"]);
    assert!(out.stderr.contains(&warning(source)), "{out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")), "the rest deploys");
    assert_eq!(fs::read_dir(&fx.root).unwrap().count(), 0, "nothing is written");
    assert!(!fx.repo.join(".rustowcache").exists());
    fx.ok(&["file", "add", source]);
    let out = fx.ok(&["apply"]);
    assert!(!out.stderr.contains("warning"), "{out:?}");
    assert!(
        out.stderr.contains(fx.root.to_str().unwrap()),
        "root target is reported: {out:?}"
    );
    let dest = fx.root_path("etc/example/config.conf");
    assert!(is_regular(&dest));
    assert_eq!(read(&dest), "plain\n");
    assert!(!fx.root_path("rustowroot").exists());
    assert!(!fx.home_path("etc").exists());
}

/// `file add` must work while other system files still wait for it.
#[test]
fn file_add_registers_one_root_file_while_others_are_unregistered() {
    let fx = Fixture::new();
    fx.write("rustowroot/etc/a.conf", "a\n");
    fx.write("rustowroot/etc/b.conf", "b\n");
    fx.ok(&["file", "add", "rustowroot/etc/a.conf"]);
    let out = fx.ok(&["apply"]);
    assert!(out.stderr.contains(&warning("rustowroot/etc/b.conf")), "{out:?}");
    assert!(is_regular(&fx.root_path("etc/a.conf")), "{out:?}");
    assert!(!fx.root_path("etc/b.conf").exists(), "{out:?}");
    fx.ok(&["file", "add", "rustowroot/etc/b.conf"]);
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.root_path("etc/a.conf")) && is_regular(&fx.root_path("etc/b.conf")));
}

/// Each deploy command and diff warn about a skipped source in their
/// selection and keep their exit code. JSON output lists the warnings;
/// -q hides them.
#[test]
fn skipped_root_file_is_a_warning_in_each_deploy_command() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    let source = "rustowroot/etc/x";
    fx.write(source, "x\n");
    let commands: [&[&str]; 6] = [
        &[],
        &["apply"],
        &["link"],
        &["file", "apply"],
        &["template", "apply"],
        &["diff"],
    ];
    for args in commands {
        let out = fx.ok(args);
        assert!(out.stderr.contains(&warning(source)), "{args:?}: {out:?}");
    }
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!fx.root_path("etc/x").exists());
    for args in [["apply", "--json"], ["diff", "--json"]] {
        let out = fx.ok(&args);
        let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
        let want = serde_json::json!([{ "source": source, "reason": reason(source) }]);
        assert_eq!(v["warnings"], want, "{args:?}: {out:?}");
        assert!(!out.stderr.contains("warning"), "{args:?}: {out:?}");
    }
    let out = fx.ok(&["apply", "-q"]);
    assert!(!out.stderr.contains("warning"), "{out:?}");
}

/// status shows a skipped source in its selection as SKIPPED, with the
/// reason; check warns about it. Neither changes its exit code.
#[test]
fn status_and_check_report_a_skipped_root_file() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    let source = "rustowroot/etc/x";
    fx.write(source, "x\n");
    fx.ok(&["apply"]);
    let row = format!("  SKIPPED {source}: {}", reason(source));
    for (args, summary) in [
        (&["status"][..], "1 ok, 1 skipped, 0 need attention"),
        (&["status", "rustowroot/"], "0 ok, 1 skipped, 0 need attention"),
    ] {
        let out = fx.ok(args);
        assert!(out.stdout.contains(&row), "{args:?}: {out:?}");
        assert!(out.stdout.lines().any(|l| l == summary), "{args:?}: {out:?}");
    }
    let out = fx.ok(&["status", ".bashrc"]);
    assert!(!out.stdout.contains("SKIPPED"), "outside the selection: {out:?}");
    assert!(out.stdout.lines().any(|l| l == "1 ok, 0 need attention"), "{out:?}");
    let out = fx.ok(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let skipped: Vec<&serde_json::Value> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "SKIPPED")
        .collect();
    assert_eq!(skipped.len(), 1, "{out:?}");
    assert_eq!(skipped[0]["source"], source);
    assert_eq!(skipped[0]["detail"], reason(source));
    let out = fx.ok(&["check"]);
    assert!(out.stderr.contains(&warning(source)), "{out:?}");
    assert!(out.stdout.contains("1 sources checked, 0 problems"), "{out:?}");
    let out = fx.ok(&["check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let want = serde_json::json!([{ "source": source, "reason": reason(source) }]);
    assert_eq!(v["warnings"], want, "{out:?}");
    assert_eq!(v["problems"], serde_json::json!([]), "{out:?}");
    fx.write(".profile.tmpl", "{{ vars.missing }}\n");
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 1, "a real problem keeps its exit code: {out:?}");
    assert!(
        out.stdout.contains("1 problems") && out.stderr.contains(&warning(source)),
        "{out:?}"
    );
}

/// A path argument that names a skipped file is an error, as for an
/// ignored file. A directory that holds one only warns.
#[test]
fn naming_a_skipped_root_file_is_an_error() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    let source = "rustowroot/etc/x";
    fx.write(source, "x\n");
    for args in [["apply", source], ["diff", source]] {
        let out = fx.run(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stderr.contains(&hint(source)), "{args:?}: {out:?}");
    }
    let out = fx.ok(&["apply", "rustowroot/etc/"]);
    assert!(out.stderr.contains(&warning(source)), "{out:?}");
    assert!(!fx.home_path(".bashrc").exists(), "outside the selection");
    let out = fx.ok(&["apply", ".bashrc"]);
    assert!(!out.stderr.contains("warning"), "outside the selection: {out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn registered_copy_maps_under_root_target() {
    let fx = Fixture::new();
    fx.write("rustowroot/etc/example/custom.conf", "copy\n");
    fx.ok(&["file", "add", "rustowroot/etc/example/custom.conf"]);
    fx.ok(&["apply"]);
    let dest = fx.root_path("etc/example/custom.conf");
    assert!(is_regular(&dest));
    assert_eq!(read(&dest), "copy\n");
}

#[test]
fn root_templates_deploy_as_copies_with_or_without_a_rule() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[vars]\nport = 8080\n[[template_rules]]\nsource = \"rustowroot/etc/example/secure.conf.tmpl\"\nmode = \"0640\"\n",
    );
    fx.write("rustowroot/etc/example/plain.conf.tmpl", "port={{ vars.port }}\n");
    fx.write("rustowroot/etc/example/secure.conf.tmpl", "secure={{ vars.port }}\n");
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("template+copy"), "{out:?}");
    let plain = fx.root_path("etc/example/plain.conf");
    assert!(is_regular(&plain));
    assert_eq!(mode(&plain), 0o644, "the default mode of a template copy");
    assert_eq!(read(&plain), "port=8080\n");
    let secure = fx.root_path("etc/example/secure.conf");
    assert!(is_regular(&secure));
    assert_eq!(mode(&secure), 0o640);
    assert_eq!(read(&secure), "secure=8080\n");
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
    fx.ok(&["status"]);
    assert_eq!(fx.run(&["diff"]).code, 0);
}

#[test]
fn root_and_home_destinations_never_mix() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/.bashrc\"\n",
    );
    fx.write("rustowroot/.bashrc", "root one\n");
    fx.write(".bashrc", "home one\n");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".bashrc")), "home one\n");
    assert_eq!(read(&fx.root_path(".bashrc")), "root one\n");
}

/// `link` and `unlink` take symlinks only, so they never touch a system
/// file; `file apply` and `template apply` deploy the root copies.
#[test]
fn commands_select_root_sources_as_copies() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/etc/a.conf\"\n",
    );
    fx.write("rustowroot/etc/a.conf", "a");
    fx.write("rustowroot/etc/t.conf.tmpl", "t");
    fx.write(".bashrc", "b");
    let out = fx.run(&["link", "rustowroot/"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("no sources this command deploys"), "{out:?}");
    fx.ok(&["link"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!fx.root_path("etc").exists());
    fx.ok(&["file", "apply"]);
    assert!(is_regular(&fx.root_path("etc/a.conf")));
    assert!(!fx.root_path("etc/t.conf").exists());
    fx.ok(&["template", "apply"]);
    assert!(is_regular(&fx.root_path("etc/t.conf")));
    let out = fx.run(&["unlink", "rustowroot/"]);
    assert_eq!(out.code, 2, "{out:?}");
    fx.ok(&["unlink"]);
    assert!(!fx.home_path(".bashrc").exists());
    assert!(is_regular(&fx.root_path("etc/a.conf")) && is_regular(&fx.root_path("etc/t.conf")));
    fx.ok(&["undeploy", "rustowroot/"]);
    assert!(!fx.root_path("etc/a.conf").exists() && !fx.root_path("etc/t.conf").exists());
}

/// A destination that you cannot read and that the state does not record
/// is a conflict: rustow cannot tell what is in it.
#[test]
fn dry_run_reports_an_unreadable_destination_as_a_conflict_and_lists_the_rest() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root can read everything");
        return;
    }
    let fx = Fixture::new();
    fx.write("rustowroot/etc/locked.conf", "x");
    fx.write("rustowroot/etc/open.conf", "y");
    fx.ok(&["file", "add", "rustowroot/etc/locked.conf", "rustowroot/etc/open.conf"]);
    let locked = common::write_file(&fx.root_path("etc/locked.conf"), "theirs");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = fx.run(&["apply", "--dry-run"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        out.stdout
            .contains("cannot read the destination; rustow has no record of it"),
        "{out:?}"
    );
    assert!(out.stdout.contains("open.conf"), "{out:?}");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
}

/// Spec Scenario E step 8: encrypted copy and template sources map under
/// the root target, with an isolated age identity.
#[test]
fn encrypted_age_sources_map_under_root_target() {
    let fx = Fixture::new();
    let key = fx.age_key("root");
    fx.age_config(
        &[&key],
        &[&key],
        "[vars]\nport = 8080\n[[files]]\nsource = \"rustowroot/etc/example/copied.conf.age\"\n\
         [[template_rules]]\nsource = \"rustowroot/etc/example/secure.conf.tmpl.age\"\nmode = \"0640\"\n",
    );
    let dir = "rustowroot/etc/example";
    fx.age_encrypt(&format!("{dir}/copied.conf.age"), "copied\n", &[&key]);
    fx.age_encrypt(
        &format!("{dir}/rendered.conf.tmpl.age"),
        "port={{ vars.port }}\n",
        &[&key],
    );
    fx.age_encrypt(
        &format!("{dir}/secure.conf.tmpl.age"),
        "secure={{ vars.port }}\n",
        &[&key],
    );
    fx.ok(&["apply"]);
    let dest = |name: &str| fx.root_path(&format!("etc/example/{name}"));
    assert!(is_regular(&dest("copied.conf")));
    assert_eq!(mode(&dest("copied.conf")), 0o600);
    assert_eq!(read(&dest("copied.conf")), "copied\n");
    assert!(is_regular(&dest("rendered.conf")));
    assert_eq!(mode(&dest("rendered.conf")), 0o600, "an encrypted template copy");
    assert_eq!(read(&dest("rendered.conf")), "port=8080\n");
    assert!(is_regular(&dest("secure.conf")));
    assert_eq!(mode(&dest("secure.conf")), 0o640);
    assert_eq!(read(&dest("secure.conf")), "secure=8080\n");
    assert!(!fx.home_path("etc").exists());
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "idempotent");
}

#[test]
fn unregistered_encrypted_root_file_is_skipped_until_added() {
    let fx = Fixture::new();
    let key = fx.age_key("root");
    fx.age_config(&[&key], &[&key], "");
    let source = "rustowroot/etc/secret.conf.age";
    fx.age_encrypt(source, "secret\n", &[&key]);
    let out = fx.ok(&["apply"]);
    assert!(out.stderr.contains(&warning(source)), "{out:?}");
    assert_eq!(fs::read_dir(&fx.root).unwrap().count(), 0, "nothing is written");
    fx.ok(&["file", "add", source]);
    fx.ok(&["apply"]);
    let dest = fx.root_path("etc/secret.conf");
    assert!(is_regular(&dest));
    assert_eq!(mode(&dest), 0o600);
    assert_eq!(read(&dest), "secret\n");
}

/// Spec Scenario E step 8 with GPG: copy sources under rustowroot/,
/// with a disposable GNUPGHOME.
#[test]
fn encrypted_gpg_sources_map_under_root_target() {
    if !require_gpg() {
        return;
    }
    let fx = Fixture::new();
    let home = fx.gpg_home("root", false);
    let fpr = home.fpr.clone();
    fx.gpg_config(
        &home,
        &[&fpr],
        "[[files]]\nsource = \"rustowroot/etc/example/copied.conf.gpg\"\n",
    );
    fx.gpg_encrypt("rustowroot/etc/example/added.conf.gpg", "gpg added\n", &home);
    fx.gpg_encrypt("rustowroot/etc/example/copied.conf.gpg", "gpg copied\n", &home);
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr.contains(&warning("rustowroot/etc/example/added.conf.gpg")),
        "{out:?}"
    );
    assert!(is_regular(&fx.root_path("etc/example/copied.conf")), "{out:?}");
    assert!(!fx.root_path("etc/example/added.conf").exists(), "{out:?}");
    fx.ok(&["file", "add", "rustowroot/etc/example/added.conf.gpg"]);
    fx.ok(&["apply"]);
    for (name, text) in [("added.conf", "gpg added\n"), ("copied.conf", "gpg copied\n")] {
        let dest = fx.root_path(&format!("etc/example/{name}"));
        assert!(is_regular(&dest), "{name}");
        assert_eq!(mode(&dest), 0o600, "{name}");
        assert_eq!(read(&dest), text);
    }
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "idempotent");
}

/// Review M1: a home source and a rustowroot source that land on the
/// same file collide, instead of the last one silently winning.
#[test]
fn home_and_root_sources_for_one_file_collide() {
    let fx = Fixture::new();
    // Put HOME inside the root target so both scopes reach one path.
    let target = fx.root.join("home/me");
    fs::create_dir_all(&target).unwrap();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/home/me/.bashrc\"\n",
    );
    fx.write(".bashrc", "home\n");
    fx.write("rustowroot/home/me/.bashrc", "root\n");
    let out = fx.run(&["--target", target.to_str().unwrap(), "apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("same destination"), "{out:?}");
    assert!(!target.join(".bashrc").exists());
}

/// `diff` only reads: a system directory the user cannot write does not
/// hide the desired content. `status` and `apply` still report the error.
#[test]
fn diff_shows_content_for_an_unwritable_system_directory() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root can write everywhere");
        return;
    }
    let fx = Fixture::new();
    fx.write("rustowroot/etc/qa.conf.tmpl", "x={{ 1 + 1 }}\n");
    let etc = fx.root_path("etc");
    fs::create_dir_all(&etc).unwrap();
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o555)).unwrap();
    let diff = fx.run(&["diff"]);
    let status = fx.run(&["status"]);
    let apply = fx.run(&["apply"]);
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(diff.code, 1, "{diff:?}");
    assert!(diff.stdout.contains("+x=2"), "{diff:?}");
    assert!(!diff.stderr.contains("cannot compute"), "{diff:?}");
    assert!(
        status.stdout.contains("MISSING"),
        "the sudo helper writes it: {status:?}"
    );
    assert_eq!(apply.code, 4, "the fixture's sudo is `false`: {apply:?}");
    assert!(apply.stdout.contains("false failed"), "{apply:?}");
    assert!(!fx.root_path("etc/qa.conf").exists());
}

/// rustow installs a system file as root. A source that another user can
/// change is a conflict for that item: `apply` writes nothing for it, and
/// `check` reports it. The trust rule is the one of link targets.
#[test]
fn a_system_source_that_others_can_change_is_a_conflict() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/etc/open.conf\"\n[[files]]\nsource = \"rustowroot/etc/ok.conf\"\n",
    );
    let open = fx.write("rustowroot/etc/open.conf", "x\n");
    fx.write("rustowroot/etc/ok.conf", "y\n");
    fs::set_permissions(&open, fs::Permissions::from_mode(0o666)).unwrap();

    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    let line = out.stdout.lines().find(|l| l.contains("open.conf")).unwrap_or_default();
    let why = format!(
        "untrusted source: {} is writable by others; another user can change what rustow installs as root",
        open.display()
    );
    assert!(line.contains(&why), "{out:?}");
    assert!(!fx.root_path("etc/open.conf").exists());
    assert_eq!(read(&fx.root_path("etc/ok.conf")), "y\n", "the other item deploys");

    let check = fx.run(&["check"]);
    assert_eq!(check.code, 1, "{check:?}");
    assert!(
        check
            .stdout
            .contains(&format!("problem: rustowroot/etc/open.conf: {why}")),
        "{check:?}"
    );
    let forced = fx.run(&["apply", "--force"]);
    assert_eq!(forced.code, 3, "--force does not resolve it: {forced:?}");
    assert!(!fx.root_path("etc/open.conf").exists());
}

/// A group-writable source is trusted only in the user's private group
/// (named like the user, no other members), as for link targets.
#[test]
fn a_group_writable_system_source_deploys_only_in_a_user_private_group() {
    use nix::unistd::{Group, User, geteuid};
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/etc/g.conf\"\n",
    );
    let src = fx.write("rustowroot/etc/g.conf", "g\n");
    let user = User::from_uid(geteuid()).unwrap().expect("test user");
    let group = Group::from_gid(user.gid).unwrap().expect("primary group");
    let private = group.name == user.name && group.mem.iter().all(|m| *m == user.name);
    std::os::unix::fs::chown(&src, None, Some(user.gid.as_raw())).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o664)).unwrap();
    let out = fx.run(&["apply"]);
    if private {
        assert_eq!(out.code, 0, "{out:?}");
        assert_eq!(read(&fx.root_path("etc/g.conf")), "g\n");
        // The private group may write the source, never the system copy.
        assert_eq!(mode(&fx.root_path("etc/g.conf")), 0o644);
    } else {
        assert_eq!(out.code, 3, "{out:?}");
        assert!(
            out.stdout.contains("untrusted source") && out.stdout.contains("chmod g-w"),
            "{out:?}"
        );
        assert!(!fx.root_path("etc/g.conf").exists());
    }
}

/// A new system file belongs to the user who writes it: in a setgid
/// directory, it does not take the directory's group.
#[test]
fn a_new_system_file_does_not_take_the_group_of_a_setgid_directory() {
    use nix::unistd::{getegid, getgroups};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/srv/proj/app.conf\"\n",
    );
    fx.write("rustowroot/srv/proj/app.conf", "a\n");
    let egid = getegid();
    let Some(other) = getgroups().unwrap().into_iter().find(|g| *g != egid) else {
        eprintln!("SKIP: the test user has no supplementary group");
        return;
    };
    let proj = fx.root_path("srv/proj");
    fs::create_dir_all(&proj).unwrap();
    std::os::unix::fs::chown(&proj, None, Some(other.as_raw())).unwrap();
    fs::set_permissions(&proj, fs::Permissions::from_mode(0o2755)).unwrap();

    fx.ok(&["apply"]);
    let gid = fs::metadata(fx.root_path("srv/proj/app.conf")).unwrap().gid();
    assert_eq!(gid, egid.as_raw(), "not group {other} of the setgid directory");
}

/// An ACL can let another user write a source whose mode bits look safe:
/// the group bits are then the ACL mask. Such a system source is untrusted.
#[cfg(target_os = "linux")]
#[test]
fn a_system_source_with_an_acl_that_lets_others_write_is_a_conflict() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/etc/acl.conf\"\n",
    );
    let src = fx.write("rustowroot/etc/acl.conf", "x\n");
    if let Err(e) = grant_write(&src, 65534) {
        eprintln!("SKIP: no POSIX ACL on this filesystem: {e}");
        return;
    }
    let why = format!(
        "untrusted source: {0} has an ACL that can give write access to other users; run `setfacl -b {0}`",
        src.display()
    );

    let check = fx.run(&["check"]);
    assert_eq!(check.code, 1, "{check:?}");
    let problem = format!("problem: rustowroot/etc/acl.conf: {why}");
    assert!(check.stdout.contains(&problem), "{check:?}");
    let out = fx.run(&["apply", "--force"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains(&why), "{out:?}");
    assert!(!fx.root_path("etc/acl.conf").exists());
}
