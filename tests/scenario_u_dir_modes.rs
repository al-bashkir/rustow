//! Directories that rustow creates get an exact mode, whatever the umask
//! or an inherited setgid bit: 0755, 0700 for private home directories and
//! encrypted home sources, 0700 for the cache and the state.

mod common;

use common::*;
use nix::sys::stat::Mode;
use std::fs;
use std::os::unix::fs::PermissionsExt;

/// No folds: each directory is a real one that rustow creates.
const NO_FOLDS: &str = "version = 1\n[defaults]\nfold_directories = false\n";

#[test]
fn new_directories_are_0755_under_umask_000() {
    let fx = Fixture::new();
    fx.write(".config/app/x", "x\n");
    fx.write(
        "rustow.toml",
        &format!("{NO_FOLDS}[[files]]\nsource = \"rustowroot/etc/app/a.conf\"\n"),
    );
    fx.write("rustowroot/etc/app/a.conf", "a\n");
    let out = fx.run_umask(Mode::empty(), &["apply"]);
    assert_eq!(out.code, 0, "{out:?}");
    for dir in [
        fx.home_path(".config"),
        fx.home_path(".config/app"),
        fx.root_path("etc"),
        fx.root_path("etc/app"),
    ] {
        assert_eq!(mode(&dir), 0o755, "{}", dir.display());
    }
}

#[test]
fn ssh_and_gnupg_are_created_private() {
    let fx = Fixture::new();
    fx.write("rustow.toml", NO_FOLDS);
    fx.write(".ssh/config", "Host x\n");
    fx.write(".gnupg/sub/gpg.conf", "x\n");
    fx.write(".config/app/x", "x\n");
    fx.ok(&["apply"]);
    for dir in [".ssh", ".gnupg", ".gnupg/sub"] {
        assert_eq!(mode(&fx.home_path(dir)), 0o700, "{dir}");
    }
    assert_eq!(mode(&fx.home_path(".config/app")), 0o755);
}

/// The file's own mode protects an encrypted system file; its directories
/// must stay open to the programs (and the group) that read it.
#[test]
fn an_encrypted_system_file_gets_0755_parents_and_a_home_one_0700() {
    let fx = Fixture::new();
    let key = fx.age_key("me");
    fx.age_config(
        &[&key],
        &[&key],
        "[[files]]\nsource = \"rustowroot/etc/sec/s.conf.age\"\nmode = \"0640\"\n",
    );
    fx.age_encrypt("rustowroot/etc/sec/s.conf.age", "root secret\n", &[&key]);
    fx.age_encrypt(".config/tok/t.age", "home secret\n", &[&key]);
    fx.ok(&["apply"]);
    assert_eq!(mode(&fx.root_path("etc/sec")), 0o755);
    assert_eq!(mode(&fx.root_path("etc/sec/s.conf")), 0o640);
    assert_eq!(mode(&fx.home_path(".config/tok")), 0o700);
}

/// On Linux a new directory inherits the setgid bit of its parent.
#[test]
fn the_cache_is_0700_under_a_setgid_parent() {
    let fx = Fixture::new();
    fs::set_permissions(&fx.repo, fs::Permissions::from_mode(0o2755)).unwrap();
    fx.write(".t.tmpl", "{{ 1 + 1 }}\n");
    fx.ok(&["apply"]);
    let cache = fx.repo.join(".rustowcache");
    assert_eq!(mode(&cache), 0o700);
    assert_eq!(mode(&cache.join("rendered")), 0o700);
    let doctor = fx.ok(&["doctor"]);
    assert!(doctor.stdout.contains(".rustowcache (0700)"), "{doctor:?}");
}

/// The missing directories above the state directory (`~/.local`,
/// `~/.local/state`) do not keep the setgid bit of a setgid home.
#[test]
fn the_state_directory_and_its_new_parents_are_0700_under_a_setgid_parent() {
    let fx = Fixture::new();
    fs::set_permissions(&fx.base, fs::Permissions::from_mode(0o2755)).unwrap();
    fx.write(".bashrc", "b\n");
    fx.ok(&["apply"]);
    assert_eq!(mode(&fx.state), 0o700, "made for the state");
    assert_eq!(mode(&fx.state.join("rustow")), 0o700);
    assert_eq!(mode(&fx.base) & 0o7777, 0o2755, "an existing directory stays as it is");
}
