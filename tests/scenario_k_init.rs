//! Spec §17.2 Scenario K — repository initialization.

mod common;

use common::*;
use std::fs;

const ENTRY: &str = "/.rustowcache/\n";

#[test]
fn new_dir_gets_minimal_config_ignores_and_private_cache() {
    let fx = Fixture::new();
    let out = fx.ok(&["init"]);
    assert!(out.stdout.contains(fx.repo.to_str().unwrap()), "{out:?}");
    let config = read(&fx.repo.join("rustow.toml"));
    assert!(
        config.contains("version = 1") && config.contains("cache_dir = \".rustowcache\""),
        "{config}"
    );
    assert_eq!(read(&fx.repo.join(".gitignore")), ENTRY);
    assert_eq!(read(&fx.repo.join(".rustowignore")), ENTRY);
    assert!(fx.repo.join(".rustowcache").is_dir());
    assert_eq!(mode(&fx.repo.join(".rustowcache")), 0o700);
}

#[test]
fn works_in_git_repo_without_touching_index() {
    let fx = Fixture::new();
    fx.write(".git/HEAD", "ref: refs/heads/main\n");
    let index = fx.write(".git/index", "binary-index");
    let before = mtime(&index);
    std::thread::sleep(std::time::Duration::from_millis(20));
    fx.ok(&["init"]);
    assert_eq!(read(&index), "binary-index");
    assert_eq!(mtime(&index), before);
    assert_eq!(fs::read_dir(fx.repo.join(".git")).unwrap().count(), 2);
}

#[test]
fn existing_config_and_ignore_rules_preserved() {
    let fx = Fixture::new();
    let config = "# my settings\n[vars]\nname = \"x\" # inline\n";
    fx.write("rustow.toml", config);
    fx.write(".gitignore", "target/\n# keep\n");
    fx.ok(&["init"]);
    let after = read(&fx.repo.join("rustow.toml"));
    assert!(after.starts_with("version = 1\n") && after.ends_with(config), "{after}");
    assert_eq!(read(&fx.repo.join(".gitignore")), format!("target/\n# keep\n{ENTRY}"));
}

#[test]
fn ignore_without_final_newline_appended_safely() {
    let fx = Fixture::new();
    fx.write(".gitignore", "node_modules");
    fx.ok(&["init"]);
    assert_eq!(read(&fx.repo.join(".gitignore")), format!("node_modules\n{ENTRY}"));
}

#[test]
fn equivalent_entries_kept_and_negation_repaired_once() {
    let fx = Fixture::new();
    fx.write(".gitignore", ".rustowcache\n");
    fx.write(".rustowignore", ".rustowcache/\n!.rustowcache/\n");
    fx.ok(&["init"]);
    assert_eq!(read(&fx.repo.join(".gitignore")), ".rustowcache\n");
    let repaired = read(&fx.repo.join(".rustowignore"));
    assert_eq!(repaired, format!(".rustowcache/\n!.rustowcache/\n{ENTRY}"));
    fx.ok(&["init"]);
    assert_eq!(read(&fx.repo.join(".rustowignore")), repaired);
}

#[test]
fn repeat_init_preserves_contents_and_mtimes() {
    let fx = Fixture::new();
    fx.ok(&["init"]);
    let files = ["rustow.toml", ".gitignore", ".rustowignore", ".rustowcache"].map(|f| fx.repo.join(f));
    let before: Vec<_> = files.iter().map(|f| (mtime(f), fs::read(f).ok())).collect();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = fx.ok(&["init"]);
    assert!(out.stdout.contains("0 changed"), "{out:?}");
    let after: Vec<_> = files.iter().map(|f| (mtime(f), fs::read(f).ok())).collect();
    assert_eq!(before, after);
}

#[test]
fn loose_cache_permissions_are_repaired() {
    let fx = Fixture::new();
    fs::create_dir(fx.repo.join(".rustowcache")).unwrap();
    fs::set_permissions(
        fx.repo.join(".rustowcache"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    fx.ok(&["init"]);
    assert_eq!(mode(&fx.repo.join(".rustowcache")), 0o700);
}

#[test]
fn malformed_config_fails_without_overwriting() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "[defaults\n");
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("rustow.toml"), "{out:?}");
    assert_eq!(read(&fx.repo.join("rustow.toml")), "[defaults\n");
    assert!(!fx.repo.join(".gitignore").exists());
    fx.write("rustow.toml", "version = 7\n");
    assert_eq!(fx.run(&["init"]).code, 2);
    assert_eq!(read(&fx.repo.join("rustow.toml")), "version = 7\n");
}

#[test]
fn configuration_errors_never_print_values() {
    for (config, place) in [
        (
            "version = 1\n[vars]\napi_token = \"TOPSECRET-1\" extra\n",
            "line 3, column 27, key vars.api_token",
        ),
        (
            "version = 1\n[vars]\napi_token = \"TOPSECRET-2\"\napi_token = \"TOPSECRET-3\"\n",
            "line 4, column 1",
        ),
        (
            "version = 1\n[defaults]\ncreate_directories = \"TOPSECRET-4\"\n",
            "line 3, column 22, key defaults.create_directories",
        ),
        ("version = \"TOPSECRET-5\"\n", "line 1, column 11, key version"),
    ] {
        let fx = Fixture::new();
        fx.write("rustow.toml", config);
        for args in [&["check"][..], &["init"], &["file", "add", "x"]] {
            let out = fx.run(args);
            assert_eq!(out.code, 2, "{args:?} {config}\n{out:?}");
            assert!(!format!("{out:?}").contains("TOPSECRET"), "{args:?} {config}\n{out:?}");
            assert!(out.stderr.contains(place), "{args:?} {config}\n{out:?}");
        }
        assert_eq!(read(&fx.repo.join("rustow.toml")), config);
    }
}

/// Sorted names in the repository root, to prove that nothing was written.
fn repo_names(fx: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(&fx.repo)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn conflicting_objects_fail_without_changes() {
    let fx = Fixture::new();
    let elsewhere = fx.base.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, fx.repo.join(".rustowcache")).unwrap();
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains(".rustowcache"), "{out:?}");
    assert!(is_symlink(&fx.repo.join(".rustowcache")));
    assert_eq!(repo_names(&fx), [".rustowcache"]);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

    let fx = Fixture::new();
    fx.write(".rustowcache", "a regular file");
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains(".rustowcache"), "{out:?}");
    assert_eq!(repo_names(&fx), [".rustowcache"]);

    let fx = Fixture::new();
    fs::create_dir(fx.repo.join(".gitignore")).unwrap();
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains(".gitignore"), "{out:?}");
    assert!(fx.repo.join(".gitignore").is_dir());
    assert_eq!(repo_names(&fx), [".gitignore"]);

    let fx = Fixture::new();
    fs::create_dir(fx.repo.join(".rustowignore")).unwrap();
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(repo_names(&fx), [".rustowignore"]);

    let fx = Fixture::new();
    fx.write("real.toml", "version = 1\n");
    std::os::unix::fs::symlink("real.toml", fx.repo.join("rustow.toml")).unwrap();
    fx.write(".rustowignore", "");
    std::os::unix::fs::symlink("nowhere", fx.repo.join(".gitignore")).unwrap();
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains("symlinks are not followed"), "{out:?}");
    assert!(is_symlink(&fx.repo.join(".gitignore")));
    assert_eq!(read(&fx.repo.join(".rustowignore")), "");
    assert_eq!(
        repo_names(&fx),
        [".gitignore", ".rustowignore", "real.toml", "rustow.toml"]
    );
}

#[test]
fn cache_dir_through_a_symlink_in_the_repo_is_refused() {
    let fx = Fixture::new();
    let outside = fx.base.join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, fx.repo.join("var")).unwrap();
    let config = "version = 1\n[defaults]\ncache_dir = \"var/cache\"\n";
    fx.write("rustow.toml", config);
    let out = fx.run(&["init"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stderr.contains("var"), "{out:?}");
    assert!(!outside.join("cache").exists());
    assert_eq!(read(&fx.repo.join("rustow.toml")), config);
    assert_eq!(repo_names(&fx), ["rustow.toml", "var"]);
    // Other commands do not create it through the symlink either.
    fx.write(".rustowignore", "/var\n");
    fx.write(".gitconfig.tmpl", "x");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!outside.join("cache").exists());
}

#[test]
fn repo_flag_and_subdirectory_resolve_root() {
    let mut fx = Fixture::new();
    let other = fx.base.join("other");
    fs::create_dir(&other).unwrap();
    fx.ok(&["--repo", other.to_str().unwrap(), "init"]);
    assert!(other.join("rustow.toml").exists());
    assert!(!fx.repo.join("rustow.toml").exists());

    fx.env_repo = false;
    fx.write(".rustowignore", "");
    fs::create_dir_all(fx.repo.join(".config/sub")).unwrap();
    fx.cwd = fx.repo.join(".config/sub");
    fx.ok(&["init"]);
    assert!(fx.repo.join("rustow.toml").exists());
    assert!(!fx.repo.join(".config/sub/rustow.toml").exists());
}

#[test]
fn custom_cache_dir_is_created_and_ignored() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\ncache_dir = \"var/cache\"\n");
    fx.ok(&["init"]);
    assert_eq!(read(&fx.repo.join(".gitignore")), "/.rustowcache/\n/var/cache/\n");
    assert_eq!(read(&fx.repo.join(".rustowignore")), "/.rustowcache/\n/var/cache/\n");
    assert_eq!(mode(&fx.repo.join("var/cache")), 0o700);
}

#[test]
fn cache_dir_with_glob_characters_is_escaped_once() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\ncache_dir = \"c[1]\"\n");
    let out = fx.ok(&["init"]);
    assert!(
        out.stdout.contains(r"/c\\[1]/"),
        "the output escapes the backslash: {out:?}"
    );
    let want = "/.rustowcache/\n/c\\[1]/\n";
    assert_eq!(read(&fx.repo.join(".gitignore")), want);
    assert_eq!(read(&fx.repo.join(".rustowignore")), want);
    fx.ok(&["init"]);
    assert_eq!(read(&fx.repo.join(".gitignore")), want);
    assert_eq!(read(&fx.repo.join(".rustowignore")), want);
}

#[test]
fn cache_dir_through_symlinked_home_or_repo_is_ignored() {
    // HOME behind a symlink, as with /home -> /var/home on Fedora Atomic.
    let fx = Fixture::new();
    let home_link = fx.base.join("home-link");
    std::os::unix::fs::symlink(&fx.home, &home_link).unwrap();
    fx.write(
        "rustow.toml",
        "version = 1\n[defaults]\ncache_dir = \"~/dotfiles/var/c\"\n",
    );
    let out = fx.run_env(&["init"], &[("HOME", home_link.to_str().unwrap())]);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(read(&fx.repo.join(".gitignore")), "/.rustowcache/\n/var/c/\n");
    assert_eq!(mode(&fx.repo.join("var/c")), 0o700);

    let fx = Fixture::new();
    let repo_link = fx.base.join("repo-link");
    std::os::unix::fs::symlink(&fx.repo, &repo_link).unwrap();
    fx.write(
        "rustow.toml",
        &format!(
            "version = 1\n[defaults]\ncache_dir = \"{}/var/c\"\n",
            repo_link.display()
        ),
    );
    fx.ok(&["init"]);
    assert_eq!(read(&fx.repo.join(".rustowignore")), "/.rustowcache/\n/var/c/\n");
}

#[test]
fn init_deploys_nothing() {
    let fx = Fixture::new();
    fx.write(".bashrc", "x");
    fx.ok(&["init"]);
    assert!(!fx.home_path(".bashrc").exists());
    assert!(!fx.state.exists());
}

#[test]
fn dry_run_writes_nothing() {
    let fx = Fixture::new();
    let out = fx.ok(&["init", "--dry-run"]);
    assert!(out.stdout.contains("create"), "{out:?}");
    assert_eq!(fs::read_dir(&fx.repo).unwrap().count(), 0);
}

/// rustow 0.1 named these files `.rustow.toml` and `.rustowroot/`. A
/// repository with an old name stops every command with exit 2: rustow
/// would ignore the old configuration and link the old system files into HOME.
#[test]
fn old_names_stop_every_command() {
    for (old, file, new) in [
        (".rustow.toml", ".rustow.toml", "rustow.toml"),
        (".rustowroot", ".rustowroot/etc/x", "rustowroot"),
    ] {
        let mut fx = Fixture::new();
        fx.write(file, "version = 1\n");
        fx.write(".bashrc", "x");
        fx.env_repo = false; // the old name alone marks the repository
        for args in [&["apply"][..], &["init"]] {
            let out = fx.run(args);
            assert_eq!(out.code, 2, "{file} {args:?}\n{out:?}");
            let want = format!("{old} is the old name of {new}; rename it");
            assert!(out.stderr.contains(&want), "{file} {args:?}\n{out:?}");
        }
        assert!(!fx.home_path(".bashrc").exists() && !fx.repo.join("rustow.toml").exists());
    }
}
