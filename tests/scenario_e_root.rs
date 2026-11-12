//! Spec §17.2 Scenario E — `.rustowroot/` mapped to a temporary root target.

mod common;

use common::*;
use std::fs;

#[test]
fn ordinary_file_maps_under_root_target() {
    let fx = Fixture::new();
    fx.write(".rustowroot/etc/example/config.conf", "plain\n");
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr.contains(fx.root.to_str().unwrap()),
        "root target is reported: {out:?}"
    );
    let dest = fx.root_path("etc/example/config.conf");
    assert!(is_symlink(&dest));
    assert_eq!(read(&dest), "plain\n");
    assert!(!fx.root_path(".rustowroot").exists());
    assert!(!fx.home_path("etc").exists());
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
fn template_link_and_metadata_copy_map_under_root_target() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[vars]\nport = 8080\n[[template_rules]]\nsource = \".rustowroot/etc/example/secure.conf.tmpl\"\nmode = \"0640\"\n",
    );
    fx.write(".rustowroot/etc/example/linked.conf.tmpl", "port={{ vars.port }}\n");
    fx.write(".rustowroot/etc/example/secure.conf.tmpl", "secure={{ vars.port }}\n");
    fx.ok(&["apply"]);
    let linked = fx.root_path("etc/example/linked.conf");
    assert!(is_symlink(&linked));
    assert_eq!(read(&linked), "port=8080\n");
    let secure = fx.root_path("etc/example/secure.conf");
    assert!(is_regular(&secure));
    assert_eq!(mode(&secure), 0o640);
    assert_eq!(read(&secure), "secure=8080\n");
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
}

#[test]
fn root_and_home_destinations_never_mix() {
    let fx = Fixture::new();
    fx.write(".rustowroot/.bashrc", "root one\n");
    fx.write(".bashrc", "home one\n");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".bashrc")), "home one\n");
    assert_eq!(read(&fx.root_path(".bashrc")), "root one\n");
}

#[test]
fn unlink_and_select_root_paths() {
    let fx = Fixture::new();
    fx.write(".rustowroot/etc/a.conf", "a");
    fx.write(".bashrc", "b");
    fx.ok(&["apply", ".rustowroot/etc/"]);
    assert!(is_symlink(&fx.root_path("etc/a.conf")));
    assert!(!fx.home_path(".bashrc").exists());
    fx.ok(&["unlink", ".rustowroot/"]);
    assert!(!fx.root_path("etc/a.conf").exists());
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
    fx.ok(&["file", "add", ".rustowroot/etc/locked.conf"]);
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

/// Review I5: a system symlink into a source that others can modify is
/// refused, as for cache-backed links.
#[test]
fn root_link_into_writable_source_tree_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    fx.write(".rustowroot/etc/x.conf", "x");
    fs::set_permissions(fx.repo.join(".rustowroot/etc"), fs::Permissions::from_mode(0o775)).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("writable by group or others"), "{out:?}");
    assert!(!fx.root_path("etc/x.conf").exists());
    fs::set_permissions(fx.repo.join(".rustowroot/etc"), fs::Permissions::from_mode(0o755)).unwrap();
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.root_path("etc/x.conf")));
}

/// Spec Scenario E step 8: encrypted link, copy and template sources map
/// under the root target, with an isolated age identity.
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
    fx.age_encrypt(&format!("{dir}/linked.conf.age"), "linked\n", &[&key]);
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
    assert!(is_symlink(&dest("linked.conf")));
    assert_eq!(read(&dest("linked.conf")), "linked\n");
    assert!(is_regular(&dest("copied.conf")));
    assert_eq!(mode(&dest("copied.conf")), 0o600);
    assert_eq!(read(&dest("copied.conf")), "copied\n");
    assert!(is_symlink(&dest("rendered.conf")));
    assert_eq!(read(&dest("rendered.conf")), "port=8080\n");
    assert!(is_regular(&dest("secure.conf")));
    assert_eq!(mode(&dest("secure.conf")), 0o640);
    assert_eq!(read(&dest("secure.conf")), "secure=8080\n");
    assert!(!fx.home_path("etc").exists());
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "idempotent");
}

/// Spec Scenario E step 8 with GPG: link and copy sources under
/// .rustowroot/, with a disposable GNUPGHOME.
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
    fx.gpg_encrypt(".rustowroot/etc/example/linked.conf.gpg", "gpg linked\n", &home);
    fx.gpg_encrypt(".rustowroot/etc/example/copied.conf.gpg", "gpg copied\n", &home);
    fx.ok(&["apply"]);
    let linked = fx.root_path("etc/example/linked.conf");
    assert!(is_symlink(&linked));
    assert_eq!(read(&linked), "gpg linked\n");
    let copied = fx.root_path("etc/example/copied.conf");
    assert!(is_regular(&copied));
    assert_eq!(mode(&copied), 0o600);
    assert_eq!(read(&copied), "gpg copied\n");
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
    fx.write(".bashrc", "home\n");
    fx.write(".rustowroot/home/me/.bashrc", "root\n");
    let out = fx.run(&["--target", target.to_str().unwrap(), "apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("same destination"), "{out:?}");
    assert!(!target.join(".bashrc").exists());
}
