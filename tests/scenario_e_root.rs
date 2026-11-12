//! Spec §17.2 Scenario E — `.rustowroot/` mapped to a temporary root target.
//! System files deploy only as copies, never as symlinks.

mod common;

use common::*;
use std::fs;
use std::path::{Path, PathBuf};

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
    let source = ".rustowroot/etc/example/config.conf";
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
    assert!(!fx.root_path(".rustowroot").exists());
    assert!(!fx.home_path("etc").exists());
}

/// `file add` must work while other system files still wait for it.
#[test]
fn file_add_registers_one_root_file_while_others_are_unregistered() {
    let fx = Fixture::new();
    fx.write(".rustowroot/etc/a.conf", "a\n");
    fx.write(".rustowroot/etc/b.conf", "b\n");
    fx.ok(&["file", "add", ".rustowroot/etc/a.conf"]);
    let out = fx.ok(&["apply"]);
    assert!(out.stderr.contains(&warning(".rustowroot/etc/b.conf")), "{out:?}");
    assert!(is_regular(&fx.root_path("etc/a.conf")), "{out:?}");
    assert!(!fx.root_path("etc/b.conf").exists(), "{out:?}");
    fx.ok(&["file", "add", ".rustowroot/etc/b.conf"]);
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
    let source = ".rustowroot/etc/x";
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

/// A path argument that names a skipped file is an error, as for an
/// ignored file. A directory that holds one only warns.
#[test]
fn naming_a_skipped_root_file_is_an_error() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    let source = ".rustowroot/etc/x";
    fx.write(source, "x\n");
    for args in [["apply", source], ["diff", source]] {
        let out = fx.run(&args);
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stderr.contains(&hint(source)), "{args:?}: {out:?}");
    }
    let out = fx.ok(&["apply", ".rustowroot/etc/"]);
    assert!(out.stderr.contains(&warning(source)), "{out:?}");
    assert!(!fx.home_path(".bashrc").exists(), "outside the selection");
    let out = fx.ok(&["apply", ".bashrc"]);
    assert!(!out.stderr.contains("warning"), "outside the selection: {out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn registered_copy_maps_under_root_target() {
    let fx = Fixture::new();
    fx.write(".rustowroot/etc/example/custom.conf", "copy\n");
    fx.ok(&["file", "add", ".rustowroot/etc/example/custom.conf"]);
    fx.ok(&["apply"]);
    let dest = fx.root_path("etc/example/custom.conf");
    assert!(is_regular(&dest));
    assert_eq!(read(&dest), "copy\n");
}

#[test]
fn root_templates_deploy_as_copies_with_or_without_a_rule() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[vars]\nport = 8080\n[[template_rules]]\nsource = \".rustowroot/etc/example/secure.conf.tmpl\"\nmode = \"0640\"\n",
    );
    fx.write(".rustowroot/etc/example/plain.conf.tmpl", "port={{ vars.port }}\n");
    fx.write(".rustowroot/etc/example/secure.conf.tmpl", "secure={{ vars.port }}\n");
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
        ".rustow.toml",
        "version = 1\n[[files]]\nsource = \".rustowroot/.bashrc\"\n",
    );
    fx.write(".rustowroot/.bashrc", "root one\n");
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
        ".rustow.toml",
        "version = 1\n[[files]]\nsource = \".rustowroot/etc/a.conf\"\n",
    );
    fx.write(".rustowroot/etc/a.conf", "a");
    fx.write(".rustowroot/etc/t.conf.tmpl", "t");
    fx.write(".bashrc", "b");
    let out = fx.run(&["link", ".rustowroot/"]);
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
    let out = fx.run(&["unlink", ".rustowroot/"]);
    assert_eq!(out.code, 2, "{out:?}");
    fx.ok(&["unlink"]);
    assert!(!fx.home_path(".bashrc").exists());
    assert!(is_regular(&fx.root_path("etc/a.conf")) && is_regular(&fx.root_path("etc/t.conf")));
    fx.ok(&["undeploy", ".rustowroot/"]);
    assert!(!fx.root_path("etc/a.conf").exists() && !fx.root_path("etc/t.conf").exists());
}

#[test]
fn dry_run_reports_unreadable_destination_and_lists_the_rest() {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root can read everything");
        return;
    }
    let fx = Fixture::new();
    fx.write(".rustowroot/etc/locked.conf", "x");
    fx.write(".rustowroot/etc/open.conf", "y");
    fx.ok(&[
        "file",
        "add",
        ".rustowroot/etc/locked.conf",
        ".rustowroot/etc/open.conf",
    ]);
    let locked = common::write_file(&fx.root_path("etc/locked.conf"), "theirs");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let out = fx.run(&["apply", "--dry-run"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(
        out.stdout.contains("Permission denied") && out.stdout.contains("privileges"),
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
        "[vars]\nport = 8080\n[[files]]\nsource = \".rustowroot/etc/example/copied.conf.age\"\n\
         [[template_rules]]\nsource = \".rustowroot/etc/example/secure.conf.tmpl.age\"\nmode = \"0640\"\n",
    );
    let dir = ".rustowroot/etc/example";
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
    let source = ".rustowroot/etc/secret.conf.age";
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

/// Spec Scenario E step 8 with GPG: copy sources under .rustowroot/,
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
        "[[files]]\nsource = \".rustowroot/etc/example/copied.conf.gpg\"\n",
    );
    fx.gpg_encrypt(".rustowroot/etc/example/added.conf.gpg", "gpg added\n", &home);
    fx.gpg_encrypt(".rustowroot/etc/example/copied.conf.gpg", "gpg copied\n", &home);
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr.contains(&warning(".rustowroot/etc/example/added.conf.gpg")),
        "{out:?}"
    );
    assert!(is_regular(&fx.root_path("etc/example/copied.conf")), "{out:?}");
    assert!(!fx.root_path("etc/example/added.conf").exists(), "{out:?}");
    fx.ok(&["file", "add", ".rustowroot/etc/example/added.conf.gpg"]);
    fx.ok(&["apply"]);
    for (name, text) in [("added.conf", "gpg added\n"), ("copied.conf", "gpg copied\n")] {
        let dest = fx.root_path(&format!("etc/example/{name}"));
        assert!(is_regular(&dest), "{name}");
        assert_eq!(mode(&dest), 0o600, "{name}");
        assert_eq!(read(&dest), text);
    }
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "idempotent");
}

/// Review M1: a home source and a .rustowroot source that land on the
/// same file collide, instead of the last one silently winning.
#[test]
fn home_and_root_sources_for_one_file_collide() {
    let fx = Fixture::new();
    // Put HOME inside the root target so both scopes reach one path.
    let target = fx.root.join("home/me");
    fs::create_dir_all(&target).unwrap();
    fx.write(
        ".rustow.toml",
        "version = 1\n[[files]]\nsource = \".rustowroot/home/me/.bashrc\"\n",
    );
    fx.write(".bashrc", "home\n");
    fx.write(".rustowroot/home/me/.bashrc", "root\n");
    let out = fx.run(&["--target", target.to_str().unwrap(), "apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("same destination"), "{out:?}");
    assert!(!target.join(".bashrc").exists());
}

/// Makes the deployed copy at `dest` look like the work of an older
/// rustow, which linked system files: a symlink to `target`, recorded with
/// the link mode `label`.
fn simulate_old_link(fx: &Fixture, dest: &Path, target: &Path, label: &str) {
    let file = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "json"))
        .unwrap();
    let mut state: serde_json::Value = serde_json::from_str(&read(&file)).unwrap();
    let record = state["entries"][dest.to_str().unwrap()].as_object_mut().unwrap();
    record.insert("mode".into(), label.into());
    record.insert("link_target".into(), target.to_str().unwrap().into());
    record.remove("meta");
    fs::write(&file, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    fs::remove_file(dest).unwrap();
    std::os::unix::fs::symlink(target, dest).unwrap();
}

/// A root template that an older rustow deployed as a symlink into the
/// cache: deploys it once with a rule, then turns it into that old link.
fn old_root_template_link(fx: &Fixture) -> (PathBuf, PathBuf) {
    let source = ".rustowroot/etc/t.conf.tmpl";
    fx.write(
        ".rustow.toml",
        &format!("version = 1\n[[template_rules]]\nsource = \"{source}\"\nmode = \"0644\"\n"),
    );
    fx.write(source, "rendered\n");
    fx.ok(&["apply"]);
    let dest = fx.root_path("etc/t.conf");
    let cache = walk(&fx.repo.join(".rustowcache/rendered"));
    assert_eq!(cache.len(), 1, "{cache:?}");
    fx.write(".rustow.toml", "version = 1\n");
    simulate_old_link(fx, &dest, &cache[0], "template+link");
    assert!(is_symlink(&dest));
    (dest, cache[0].clone())
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn old_root_template_link_becomes_a_copy() {
    let fx = Fixture::new();
    let (dest, _) = old_root_template_link(&fx);
    let out = fx.run(&["status"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("OUTDATED"), "{out:?}");
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert!(is_regular(&dest));
    assert_eq!(mode(&dest), 0o644);
    assert_eq!(read(&dest), "rendered\n");
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
}

/// An edit made through the old link lives in the cache entry. It must
/// survive the switch to a copy: a conflict first, then `--backup` saves it.
#[test]
fn old_root_template_link_with_an_edited_cache_entry_is_a_conflict() {
    let fx = Fixture::new();
    let (dest, cache) = old_root_template_link(&fx);
    fs::write(&cache, "EDIT\n").unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        out.stdout.contains("conflict") && out.stdout.contains("cache entry"),
        "{out:?}"
    );
    assert!(is_symlink(&dest), "nothing changed");
    assert_eq!(read(&cache), "EDIT\n");
    fx.ok(&["apply", "--backup"]);
    assert!(is_regular(&dest));
    assert_eq!(read(&dest), "rendered\n");
    let backups = walk(&fx.state.join("rustow/backups"));
    assert!(
        backups.iter().any(|b| read(b) == "EDIT\n"),
        "the edit is saved: {backups:?}"
    );
}

/// An older rustow linked an unregistered system file to its source. It
/// becomes a copy after `rustow file add`; until then nothing changes.
#[test]
fn old_root_plain_link_becomes_a_copy_after_file_add() {
    let fx = Fixture::new();
    let source = ".rustowroot/etc/p.conf";
    fx.write(
        ".rustow.toml",
        &format!("version = 1\n[[files]]\nsource = \"{source}\"\n"),
    );
    let abs = fx.write(source, "plain\n");
    fx.ok(&["apply"]);
    let dest = fx.root_path("etc/p.conf");
    fx.write(".rustow.toml", "version = 1\n");
    simulate_old_link(&fx, &dest, &abs, "link");
    let out = fx.ok(&["apply"]);
    assert!(out.stderr.contains(&warning(source)), "{out:?}");
    assert!(!out.stdout.contains("orphaned"), "the source still exists: {out:?}");
    assert!(is_symlink(&dest));
    fx.ok(&["file", "add", source]);
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert!(is_regular(&dest));
    assert_eq!(read(&dest), "plain\n");
}
