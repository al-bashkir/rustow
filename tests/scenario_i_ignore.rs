//! Spec §17.2 Scenario I — ignore rules and built-in exclusions.

mod common;

use common::*;
use std::fs;

#[test]
fn reserved_files_never_deployed_even_with_negations() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\nfold_directories = true\n");
    fx.write(
        ".rustowignore",
        "!.rustowcache/\n!.rustowcache/**\n!.gitignore\n!.git\n",
    );
    fx.write(".gitignore", "x\n");
    fx.write(".git/HEAD", "ref\n");
    fx.write(".rustowcache/rendered/x", "cache");
    fx.write(".bashrc", "b");
    fx.ok(&["apply"]);
    for name in ["rustow.toml", ".rustowignore", ".gitignore", ".git", ".rustowcache"] {
        assert!(
            fs::symlink_metadata(fx.home_path(name)).is_err(),
            "{name} must not be deployed"
        );
    }
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn cache_is_excluded_without_init_and_cannot_be_selected() {
    let fx = Fixture::new();
    fx.write(".rustowcache/rendered/x", "cache");
    fx.write(".bashrc", "b");
    fx.ok(&["apply"]);
    assert!(!fx.home_path(".rustowcache").exists());
    for path in [".rustowcache/rendered/x", ".rustowcache", ".rustowcache/"] {
        let out = fx.run(&["apply", path]);
        assert_eq!(out.code, 2, "{path}: {out:?}");
        assert!(out.stderr.contains("reserved"), "{out:?}");
    }
}

#[test]
fn configured_in_repo_cache_and_state_excluded() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[defaults]\ncache_dir = \"var/cache\"\nstate_dir = \"var/state\"\n",
    );
    fx.write("var/cache/rendered/old", "c");
    fx.write("var/state/0123456789abcdef.json", "s");
    fx.write("var/other", "deployable");
    fx.ok(&["apply"]);
    assert!(!fx.home_path("var/cache").exists());
    assert!(!fx.home_path("var/state").exists());
    assert!(is_symlink(&fx.home_path("var/other")));
}

/// The built-in .rustowcache is reserved by name and hides no source tree,
/// so an OS file in it (Finder's .DS_Store) must not block commands.
#[test]
fn foreign_file_in_default_cache_does_not_block_commands() {
    let fx = Fixture::new();
    fx.write(".bashrc", "a\n");
    fx.write(".rustowcache/.DS_Store", "x");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!fx.home_path(".rustowcache").exists());
}

#[test]
fn custom_patterns_and_negation() {
    let fx = Fixture::new();
    fx.write(
        ".rustowignore",
        "# comment\n*.bak\n!keep.bak\nprivate/\n.config/example/local.conf\n",
    );
    fx.write("a.bak", "x");
    fx.write("keep.bak", "x");
    fx.write("private/key", "x");
    fx.write(".config/example/local.conf", "x");
    fx.write(".config/example/shared.conf", "x");
    fx.ok(&["apply"]);
    assert!(!fx.home_path("a.bak").exists());
    assert!(is_symlink(&fx.home_path("keep.bak")));
    assert!(!fx.home_path("private").exists());
    assert!(!fx.home_path(".config/example/local.conf").exists());
    assert!(is_symlink(&fx.home_path(".config/example/shared.conf")));
}

/// Each ignore pattern costs memory, so rustow reads at most 10000 of
/// them: from `.rustowignore`, and from all `[[global_rules]]` together.
/// Comments and blank lines do not count.
#[test]
fn more_than_ten_thousand_ignore_patterns_are_a_configuration_error() {
    let fx = Fixture::new();
    let patterns = |n: usize| (0..n).map(|i| format!("a{i}/**/b*.y\n")).collect::<String>();
    fx.write(".rustowignore", &format!("# many\n\n{}", patterns(10_000)));
    fx.write("a.bak", "x");
    fx.ok(&["check"]);
    fx.write(".rustowignore", &patterns(10_001));
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr
            .contains(".rustowignore: 10001 patterns; rustow reads at most 10000"),
        "{out:?}"
    );

    fx.write(".rustowignore", "");
    let list = (0..10_001).map(|i| format!("\"x{i}\"")).collect::<Vec<_>>().join(",");
    fx.write(
        "rustow.toml",
        &format!("version = 1\n[[global_rules]]\nos = \"plan9\"\nignore = [{list}]\n"),
    );
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr
            .contains("[[global_rules]] ignore: 10001 patterns; rustow reads at most 10000"),
        "{out:?}"
    );
}

#[test]
fn ignore_applies_to_root_relative_files_and_templates() {
    let fx = Fixture::new();
    // The ignored system file is not registered: it is skipped before the copy-only check.
    fx.write(".rustowignore", "/rustowroot/etc/secret.conf\n*.tmpl\n");
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/etc/public.conf\"\n",
    );
    fx.write("rustowroot/etc/secret.conf", "s");
    fx.write("rustowroot/etc/public.conf", "p");
    fx.write(".gitconfig.tmpl", "{{ undefined_would_fail }}");
    fx.ok(&["apply"]);
    assert!(!fx.root_path("etc/secret.conf").exists());
    assert!(is_regular(&fx.root_path("etc/public.conf")));
    assert!(!fx.home_path(".gitconfig").exists());
}

#[test]
fn internal_dir_that_overlaps_sources_is_a_configuration_error() {
    for key in ["cache_dir", "state_dir"] {
        let fx = Fixture::new();
        fx.write(
            "rustow.toml",
            &format!("version = 1\n[defaults]\n{key} = \".config\"\n"),
        );
        fx.write(".config/nvim/init.lua", "x");
        for cmd in ["check", "status", "apply", "init"] {
            let out = fx.run(&[cmd]);
            assert_eq!(out.code, 2, "{key} {cmd}: {out:?}");
            assert!(
                out.stderr.contains(key) && out.stderr.contains("nvim"),
                "{key} {cmd}: {out:?}"
            );
        }
        assert!(!fx.home_path(".config").exists());
    }
    for dir in [".", "./", "~/dotfiles", "rustowroot", "rustowroot/c", ".git/c"] {
        let fx = Fixture::new();
        fx.write(
            "rustow.toml",
            &format!("version = 1\n[defaults]\ncache_dir = \"{dir}\"\n"),
        );
        let out = fx.run(&["check"]);
        assert_eq!(out.code, 2, "{dir}: {out:?}");
        assert!(out.stderr.contains("cache_dir"), "{dir}: {out:?}");
    }
}

#[test]
fn internal_dir_may_hold_rustow_entries() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[defaults]\ncache_dir = \"var\"\nstate_dir = \"var\"\n",
    );
    for name in [
        "rendered/a/b/x",
        "decrypted/a/b/y",
        "edit/edit-Ab12Cd/.lock",
        "backups/0123456789abcdef/x",
        "cache/rendered/z",
        "0123456789abcdef.json",
        "0123456789abcdef.lock",
        ".rustow-tmp-Ab12Cd",
    ] {
        fx.write(&format!("var/{name}"), "x");
    }
    fx.write(".bashrc", "b");
    fx.ok(&["check"]);
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!fx.home_path("var").exists());
}

#[test]
fn in_repo_cache_named_through_a_symlink_is_excluded() {
    let fx = Fixture::new();
    let link = fx.base.join("repo-link");
    std::os::unix::fs::symlink(&fx.repo, &link).unwrap();
    fx.write(
        "rustow.toml",
        &format!("version = 1\n[defaults]\ncache_dir = \"{}/var/c\"\n", link.display()),
    );
    fx.write("var/c/rendered/x", "cache");
    fx.write(".bashrc", "b");
    let out = fx.ok(&["apply", "--dry-run"]);
    assert!(!out.stdout.contains("var/c"), "{out:?}");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(fs::symlink_metadata(fx.home_path("var")).is_err());
}

#[test]
fn leftover_rustow_temp_files_are_reserved_at_any_depth() {
    let fx = Fixture::new();
    fx.write(".rustowignore", "!.rustow-tmp-*\n!**/.rustow-tmp-*\n");
    fx.write(".rustow-tmp-Ab12Cd", "half-written .gitignore");
    fx.write(".config/app/.rustow-tmp-Xy34Zw", "half-written secret");
    fx.write(".config/app/settings", "s");
    fx.ok(&["apply"]);
    assert!(fs::symlink_metadata(fx.home_path(".rustow-tmp-Ab12Cd")).is_err());
    assert!(fs::symlink_metadata(fx.home_path(".config/app/.rustow-tmp-Xy34Zw")).is_err());
    assert!(is_symlink(&fx.home_path(".config/app/settings")));
    let out = fx.run(&["apply", ".config/app/.rustow-tmp-Xy34Zw"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("reserved"), "{out:?}");
}

#[test]
fn status_lists_ignored_on_request() {
    let fx = Fixture::new();
    fx.write(".rustowignore", "*.bak\n");
    fx.write("a.bak", "x");
    fx.write(".bashrc", "b\n");
    fx.ok(&["apply"]);
    let out = fx.run(&["status", "--ignored"]);
    assert!(
        out.stdout.contains("IGNORED") && out.stdout.contains("a.bak"),
        "{out:?}"
    );
    assert_eq!(
        out.stdout.lines().last(),
        Some("1 ok, 1 ignored, 0 need attention"),
        "{out:?}"
    );
    fx.write("n.age.gpg", "x");
    let out = fx.run(&["status", "--ignored"]);
    let summary = "1 ok, 1 skipped, 1 ignored, 0 need attention";
    assert_eq!(out.stdout.lines().last(), Some(summary), "{out:?}");
}

/// Only a configured `state_dir` may lie inside the repository: one from
/// XDG_STATE_HOME changes with the environment, and a run with another
/// value would deploy its files as sources.
#[test]
fn a_state_directory_from_the_environment_inside_the_repository_is_refused() {
    let fx = Fixture::new();
    fx.write(".bashrc", "b\n");
    let out = fx.run_env(&["apply"], &[("XDG_STATE_HOME", fx.repo.to_str().unwrap())]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains("is inside the repository") && out.stderr.contains("XDG_STATE_HOME"),
        "{out:?}"
    );
    assert!(!fx.repo.join("rustow").exists(), "nothing written there");
    assert!(!fx.home_path(".bashrc").exists());
}
