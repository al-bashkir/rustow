//! Shell completion: the shell asks the binary on each TAB, and PATH
//! arguments complete repository-relative sources, dotfiles included.

mod common;

use common::*;

fn repo() -> Fixture {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n");
    fx.write(".rustowignore", "*.bak\n");
    fx.write(".bashrc", "b\n");
    fx.write(".bashrc.bak", "old\n");
    fx.write(".token.age", "x\n");
    fx.write(".config/app/a.conf", "a\n");
    fx.write(".config/app/s.gpg", "s\n");
    fx.write(".git/config", "g\n");
    fx
}

/// The candidates for the last word, as the zsh script asks for them,
/// without options and descriptions.
fn complete(fx: &Fixture, words: &[&str]) -> Vec<String> {
    let mut args = vec!["--", "rustow"];
    args.extend_from_slice(words);
    let index = words.len().to_string();
    let out = fx.run_env(
        &args,
        &[
            ("RUSTOW_COMPLETE", "zsh"),
            ("_CLAP_COMPLETE_INDEX", &index),
            ("_CLAP_IFS", "\n"),
        ],
    );
    assert_eq!(out.code, 0, "{words:?}: {out:?}");
    out.stdout
        .lines()
        .filter(|l| !l.starts_with('-'))
        .map(|l| l.split(':').next().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn path_arguments_complete_repository_sources() {
    let mut fx = repo();
    fx.cwd = fx.home.clone(); // outside the repository: still repository-relative
    assert_eq!(complete(&fx, &["apply", ""]), [".bashrc", ".config/", ".token.age"]);
    assert_eq!(complete(&fx, &["apply", ".c"]), [".config/"]);
    assert_eq!(complete(&fx, &["undeploy", ".config/"]), [".config/app/"]);
    assert_eq!(
        complete(&fx, &["file", "add", ".config/app/"]),
        [".config/app/a.conf", ".config/app/s.gpg"]
    );
    assert!(complete(&fx, &["apply", "../"]).is_empty());
    assert!(complete(&fx, &["apply", ".bashrc/"]).is_empty(), "not a directory");
}

#[test]
fn outside_a_repository_paths_complete_nothing() {
    let mut fx = repo();
    fx.env_repo = false;
    fx.cwd = fx.base.clone();
    assert!(complete(&fx, &["apply", ""]).is_empty());
}

#[test]
fn enc_completes_plaintext_and_enc_edit_encrypted_files() {
    let fx = repo();
    let enc = complete(&fx, &["enc", ""]);
    assert!(
        enc.contains(&".bashrc".to_owned()) && enc.contains(&"edit".to_owned()),
        "{enc:?}"
    );
    assert!(!enc.contains(&".token.age".to_owned()), "{enc:?}");
    assert_eq!(complete(&fx, &["enc", ".config/app/"]), [".config/app/a.conf"]);
    assert_eq!(complete(&fx, &["enc", "edit", ""]), [".config/", ".token.age"]);
    assert_eq!(complete(&fx, &["enc", "edit", ".config/app/"]), [".config/app/s.gpg"]);
}

#[test]
fn completions_print_a_script_that_calls_rustow() {
    let fx = Fixture::new();
    let zsh = fx.ok(&["completions", "zsh"]).stdout;
    assert!(zsh.starts_with("#compdef rustow"), "{zsh}");
    assert!(zsh.contains("RUSTOW_COMPLETE=\"zsh\""), "{zsh}");
    // Installed in fpath as _rustow, the first TAB must complete, not only register.
    assert!(zsh.contains("[ \"$funcstack[1]\" = \"_rustow\" ]"), "{zsh}");
    for shell in ["bash", "fish"] {
        let script = fx.ok(&["completions", shell]).stdout;
        assert!(script.contains("RUSTOW_COMPLETE"), "{shell}: {script}");
    }
}
