//! Spec §17.2 Scenario I — ignore rules and built-in exclusions.

mod common;

use common::*;
use std::fs;

#[test]
fn reserved_files_never_deployed_even_with_negations() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n[defaults]\nfold_directories = true\n");
    fx.write(
        ".rustowignore",
        "!.rustowcache/\n!.rustowcache/**\n!.gitignore\n!.git\n",
    );
    fx.write(".gitignore", "x\n");
    fx.write(".git/HEAD", "ref\n");
    fx.write(".rustowcache/rendered/x", "cache");
    fx.write(".bashrc", "b");
    fx.ok(&["apply"]);
    for name in [".rustow.toml", ".rustowignore", ".gitignore", ".git", ".rustowcache"] {
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
        ".rustow.toml",
        "version = 1\n[defaults]\ncache_dir = \"var/cache\"\nstate_dir = \"var/state\"\n",
    );
    fx.write("var/cache/old", "c");
    fx.write("var/state/old.json", "s");
    fx.write("var/other", "deployable");
    fx.ok(&["apply"]);
    assert!(!fx.home_path("var/cache").exists());
    assert!(!fx.home_path("var/state").exists());
    assert!(is_symlink(&fx.home_path("var/other")));
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

#[test]
fn ignore_applies_to_root_relative_files_and_templates() {
    let fx = Fixture::new();
    fx.write(".rustowignore", "/.rustowroot/etc/secret.conf\n*.tmpl\n");
    fx.write(".rustowroot/etc/secret.conf", "s");
    fx.write(".rustowroot/etc/public.conf", "p");
    fx.write(".gitconfig.tmpl", "{{ undefined_would_fail }}");
    fx.ok(&["apply"]);
    assert!(!fx.root_path("etc/secret.conf").exists());
    assert!(is_symlink(&fx.root_path("etc/public.conf")));
    assert!(!fx.home_path(".gitconfig").exists());
}

#[test]
fn status_lists_ignored_on_request() {
    let fx = Fixture::new();
    fx.write(".rustowignore", "*.bak\n");
    fx.write("a.bak", "x");
    let out = fx.run(&["status", "--ignored"]);
    assert!(
        out.stdout.contains("IGNORED") && out.stdout.contains("a.bak"),
        "{out:?}"
    );
}
