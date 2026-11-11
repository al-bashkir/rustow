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
