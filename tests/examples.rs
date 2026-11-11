//! The example repository must pass `check` and plan a clean `apply`.

use std::path::Path;
use std::process::{Command, Output};

fn rustow(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustow"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_STATE_HOME", home)
        .arg("--repo")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/dotfiles"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn example_repository_is_valid() {
    let tmp = tempfile::tempdir().unwrap();
    let out = rustow(tmp.path(), &["check"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("0 problems"), "{text}");

    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let out = rustow(
        tmp.path(),
        &["--target", tmp.path().to_str().unwrap(), "--root-target", root.to_str().unwrap(), "apply", "--dry-run"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}
