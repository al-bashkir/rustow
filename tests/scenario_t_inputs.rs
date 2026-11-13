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
