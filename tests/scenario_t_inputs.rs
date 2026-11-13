//! Configuration, ignore and state files that are not ordinary files, or
//! that are too large, stop the command with exit 2. rustow never waits on
//! a FIFO or reads a device without end. Each run is killed after a few
//! seconds and gets 1 GiB of memory, so a regression fails instead of hanging.

mod common;

use common::*;
use std::fs;
use std::path::{Path, PathBuf};

fn fifo(path: &Path) {
    nix::unistd::mkfifo(path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
}

fn refused(fx: &Fixture, args: &[&str], name: &str, why: &str) {
    let out = fx.run_within(5, args);
    assert_eq!(out.code, 2, "{args:?}\n{out:?}");
    assert!(out.stderr.contains(name) && out.stderr.contains(why), "{out:?}");
}

#[test]
fn a_fifo_as_configuration_or_ignore_file_is_refused() {
    for name in ["rustow.toml", ".rustowignore"] {
        let fx = Fixture::new();
        fx.write(".bashrc", "b\n");
        fifo(&fx.repo.join(name));
        refused(&fx, &["check"], name, "is not a regular file");
        refused(&fx, &["apply"], name, "is not a regular file");
    }
}

/// A file that rustow cannot read is a configuration error too: it names
/// the file and the cause.
#[test]
fn an_unreadable_configuration_or_ignore_file_is_refused() {
    if nix::unistd::geteuid().is_root() {
        return; // root reads it anyway
    }
    for name in ["rustow.toml", ".rustowignore"] {
        let fx = Fixture::new();
        fx.write(".bashrc", "b\n");
        let file = fx.write(name, "\n");
        fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
        refused(&fx, &["check"], name, "Permission denied");
        refused(&fx, &["status"], name, "Permission denied");
    }
}

/// A symlink that does not resolve is not a missing file: the defaults
/// (no rules) would deploy into the wrong target, or deploy ignored files.
#[test]
fn a_dangling_symlink_as_configuration_or_ignore_file_is_refused() {
    for name in ["rustow.toml", ".rustowignore"] {
        let fx = Fixture::new();
        fx.write(".bashrc", "b\n");
        std::os::unix::fs::symlink("../hosts/laptop.toml", fx.repo.join(name)).unwrap();
        refused(&fx, &["check"], name, "is a symlink that does not resolve");
        refused(&fx, &["apply"], name, "is a symlink that does not resolve");
        assert!(!fx.home_path(".bashrc").exists(), "{name}");
    }
}

/// Every command accepts a symlink to a regular file. The commands that
/// edit the file (`init`, `file remove`) change the file that the symlink
/// names, and the symlink stays.
#[test]
fn edits_of_a_symlinked_configuration_change_the_file_it_names() {
    let fx = Fixture::new();
    let hosts = fx.base.join("hosts");
    fs::create_dir(&hosts).unwrap();
    fs::write(hosts.join("laptop.toml"), "# mine\n[[files]]\nsource = \"a.conf\"\n").unwrap();
    fs::write(hosts.join("ignore"), "").unwrap();
    std::os::unix::fs::symlink("../../hosts/laptop.toml", fx.repo.join("rustow.toml")).unwrap();
    std::os::unix::fs::symlink("../../hosts/ignore", fx.repo.join(".rustowignore")).unwrap();
    fx.write("a.conf", "a\n");
    fx.ok(&["init"]);
    let out = fx.ok(&["file", "remove", "a.conf"]);
    assert!(out.stdout.contains("unregister a.conf"), "{out:?}");
    for name in ["rustow.toml", ".rustowignore"] {
        assert!(is_symlink(&fx.repo.join(name)), "{name}");
    }
    let config = read(&hosts.join("laptop.toml"));
    assert!(
        config.starts_with("version = 1\n") && config.contains("# mine\n"),
        "{config}"
    );
    assert!(!config.contains("a.conf"), "{config}");
    assert!(read(&hosts.join("ignore")).contains("/.rustowcache/"));
}

#[test]
fn a_device_or_directory_as_configuration_or_ignore_file_is_refused() {
    for name in ["rustow.toml", ".rustowignore"] {
        let fx = Fixture::new();
        fx.write(".bashrc", "b\n");
        std::os::unix::fs::symlink("/dev/zero", fx.repo.join(name)).unwrap();
        refused(&fx, &["status"], name, "is not a regular file");
        fs::remove_file(fx.repo.join(name)).unwrap();
        fs::create_dir(fx.repo.join(name)).unwrap();
        refused(&fx, &["status"], name, "is not a regular file");
    }
}

#[test]
fn a_configuration_file_over_16_mib_is_refused() {
    let fx = Fixture::new();
    let file = fs::File::create(fx.repo.join("rustow.toml")).unwrap();
    file.set_len((16 << 20) + 1).unwrap();
    refused(&fx, &["check"], "rustow.toml", "larger than 16 MiB");
}

fn state_file(fx: &Fixture) -> PathBuf {
    fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "json"))
        .unwrap()
}

/// rustow hashes a copy source in small pieces: a source larger than the
/// memory that rustow may use still plans.
#[cfg(target_os = "linux")]
#[test]
fn a_copy_source_larger_than_memory_plans() {
    let fx = Fixture::new();
    let big = fs::File::create(fx.repo.join(".big")).unwrap();
    big.set_len(3 << 29).unwrap(); // 1.5 GiB, sparse; rustow gets 1 GiB
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \".big\"\n");
    let out = fx.run_within(60, &["status"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("MISSING ~/.big"), "{out:?}");
}

#[test]
fn a_fifo_as_state_file_is_refused() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    fx.ok(&["apply"]);
    let state = state_file(&fx);
    fs::remove_file(&state).unwrap();
    fifo(&state);
    refused(&fx, &["status"], &state.display().to_string(), "is not a regular file");
}

/// rustow never follows a symlink at the state file: it is not a regular
/// file either, wherever it points.
#[test]
fn a_symlink_as_state_file_is_refused() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    fx.ok(&["apply"]);
    let state = state_file(&fx);
    let copy = fx.base.join("state-copy.json");
    fs::rename(&state, &copy).unwrap();
    for target in [Path::new("/dev/zero"), copy.as_path()] {
        std::os::unix::fs::symlink(target, &state).unwrap();
        refused(&fx, &["status"], &state.display().to_string(), "is not a regular file");
        fs::remove_file(&state).unwrap();
    }
}

/// Every command refuses a symlink at the state directory, as `apply`
/// does; a symlink above it is followed.
#[test]
fn a_symlink_as_state_directory_is_refused() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    fx.ok(&["apply"]);
    let (dir, real) = (fx.state.join("rustow"), fx.base.join("real"));
    fs::rename(&dir, &real).unwrap();
    std::os::unix::fs::symlink(&real, &dir).unwrap();
    for args in [&["status"][..], &["diff"], &["check"], &["apply"], &["state", "export"]] {
        let out = fx.run(args);
        assert_eq!(out.code, 4, "{args:?}\n{out:?}");
        let name = dir.display().to_string();
        assert!(
            out.stderr.contains(&name) && out.stderr.contains("(symlinks are not followed)"),
            "{args:?}\n{out:?}"
        );
    }
    fs::remove_file(&dir).unwrap();
    fs::rename(&real, &dir).unwrap();
    fs::rename(&fx.state, fx.base.join("state-real")).unwrap();
    std::os::unix::fs::symlink(fx.base.join("state-real"), &fx.state).unwrap();
    assert!(fx.ok(&["status", "-v"]).stdout.contains("LINKED"), "a symlink above");
}

#[test]
fn state_import_of_a_device_or_fifo_is_refused() {
    let fx = Fixture::new();
    refused(
        &fx,
        &["state", "import", "/dev/zero"],
        "/dev/zero",
        "is not a regular file",
    );
    let pipe = fx.base.join("pipe");
    fifo(&pipe);
    refused(
        &fx,
        &["state", "import", pipe.to_str().unwrap()],
        "pipe",
        "is not a regular file",
    );
}
