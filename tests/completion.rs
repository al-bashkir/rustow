//! Shell completion: the shell asks the binary on each TAB, and PATH
//! arguments complete repository-relative sources, dotfiles included.

mod common;

use common::*;

fn repo() -> Fixture {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
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
    candidates(fx, words)
        .into_iter()
        .filter(|c| !c.starts_with('-'))
        .collect()
}

/// The option candidates for the last word, in the order the shell shows them.
fn options(fx: &Fixture, words: &[&str]) -> Vec<String> {
    candidates(fx, words)
        .into_iter()
        .filter(|c| c.starts_with('-'))
        .collect()
}

fn candidates(fx: &Fixture, words: &[&str]) -> Vec<String> {
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
        .map(|l| l.split(':').next().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn conflict_options_complete_only_where_they_resolve_conflicts() {
    let fx = repo();
    let has = |words: &[&str], o: &str| options(&fx, words).contains(&o.to_owned());
    for words in [&["--"][..], &["apply", "--"], &["file", "apply", "--"]] {
        assert!(has(words, "--backup") && has(words, "--on-conflict"), "{words:?}");
    }
    assert!(has(&["unlink", "--"], "--on-conflict") && !has(&["unlink", "--"], "--force"));
    assert!(has(&["state", "export", "--"], "--force"));
    for words in [&["status", "--"][..], &["enc", "--"], &["state", "export", "--"]] {
        assert!(!has(words, "--backup") && !has(words, "--on-conflict"), "{words:?}");
    }
}

/// Options complete only after the word starts with `-`; before that,
/// commands and paths complete, and an empty list stays empty.
#[test]
fn options_complete_only_after_a_dash() {
    let fx = repo();
    for words in [
        &[""][..],
        &["file", ""],
        &["apply", ""],
        &["check", ""],
        &["apply", ".c"],
    ] {
        assert_eq!(options(&fx, words), Vec::<String>::new(), "{words:?}");
    }
    assert!(complete(&fx, &[""]).contains(&"apply".to_owned()));
    assert!(complete(&fx, &["file", ""]).contains(&"add".to_owned()));
    assert!(options(&fx, &["file", "add", "-"]).contains(&"-n".to_owned()));
    assert!(options(&fx, &["apply", "--b"]).contains(&"--backup".to_owned()));
    // fish sends no index: the last word is the current one.
    let fish = fx.run_env(&["--", "rustow", "file", ""], &[("RUSTOW_COMPLETE", "fish")]);
    assert!(fish.stdout.lines().any(|l| l.starts_with("add")), "{fish:?}");
    assert!(!fish.stdout.lines().any(|l| l.starts_with('-')), "{fish:?}");
    let fish = fx.run_env(&["--", "rustow", "file", "add", "-"], &[("RUSTOW_COMPLETE", "fish")]);
    assert!(fish.stdout.lines().any(|l| l.starts_with("-n")), "{fish:?}");
}

/// A command shows, completes and takes only the global options it reads.
#[test]
fn global_options_show_only_where_a_command_reads_them() {
    let fx = repo();
    for (words, shown, hidden) in [
        (
            &["apply", "-"][..],
            &["-n", "-i", "-v", "--no-color", "-R"][..],
            &[][..],
        ),
        (&["status", "-"], &["-i", "-v", "-j", "-R"], &["-n", "--no-color"]),
        (&["enc", "-"], &["-i", "-q"], &["-n", "-v"]),
        (&["file", "add", "-"], &["-n", "-t"], &["-i", "-v"]),
        (&["check", "-"], &["-j", "-r"], &["-n", "-i", "-v", "--no-color"]),
        (&["completions", "-"], &[], &["-r", "-t", "-R", "-n", "-j", "-q"]),
    ] {
        let opts = options(&fx, words);
        assert!(
            shown.iter().all(|o| opts.contains(&o.to_string())),
            "{words:?}: {opts:?}"
        );
        assert!(
            !hidden.iter().any(|o| opts.contains(&o.to_string())),
            "{words:?}: {opts:?}"
        );
    }
    let help = fx.ok(&["check", "--help"]).stdout;
    assert!(help.contains("--json") && !help.contains("--dry-run"), "{help}");
    assert!(!fx.ok(&["completions", "--help"]).stdout.contains("Global options"));
    for (args, refused) in [
        (&["status", "-n"][..], "`rustow status` does not take --dry-run"),
        (&["-n", "status"], "`rustow status` does not take --dry-run"),
        (&["check", "-v"], "`rustow check` does not take --verbose"),
        (
            &["file", "list", "--no-color"],
            "`rustow file list` does not take --no-color",
        ),
        (
            &["completions", "-j", "zsh"],
            "`rustow completions` does not take --json",
        ),
    ] {
        let out = fx.run(args);
        assert_eq!(out.code, 2, "{args:?}\n{out:?}");
        assert!(out.stderr.contains(refused), "{out:?}");
    }
    fx.ok(&["-n", "link"]);
}

#[test]
fn options_of_the_command_come_before_global_options() {
    let fx = repo();
    for (words, own) in [
        (&["enc", "--"][..], "--backend"),
        (&["enc", "edit", "--"], "--reencrypt"),
        (&["status", "--"], "--ignored"),
        (&["undeploy", "--"], "--yes"),
    ] {
        let opts = options(&fx, words);
        let at = |o: &str| {
            opts.iter()
                .position(|c| c == o)
                .unwrap_or_else(|| panic!("{o}: {opts:?}"))
        };
        assert!(at(own) < at("--repo"), "{words:?}: {opts:?}");
    }
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
