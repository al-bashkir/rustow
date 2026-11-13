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
