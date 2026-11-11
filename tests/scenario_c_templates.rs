//! Spec §17.2 Scenario C — templates rendered into the cache and symlinked.

mod common;

use common::*;
use std::fs;
use std::path::{Path, PathBuf};

const TEMPLATE: &str = "[user]\n    name = {{ vars.git.user_name }}\n[core]\n    editor = {{ vars.editor }}\n";

fn config(name: &str) -> String {
    format!("version = 1\n[vars]\neditor = \"nvim\"\n[vars.git]\nuser_name = \"{name}\"\n")
}

fn templated() -> Fixture {
    let fx = Fixture::new();
    fx.write(".rustow.toml", &config("Example User"));
    fx.write(".gitconfig.tmpl", TEMPLATE);
    fx
}

/// The cache file behind a deployed template symlink.
fn cache_target(fx: &Fixture, dest: &str) -> PathBuf {
    let link = fx.home_path(dest);
    let target = link.parent().unwrap().join(link_text(&link));
    target.canonicalize().unwrap()
}

#[test]
fn scenario_c_full_flow() {
    let fx = templated();
    fx.ok(&["template", "apply"]);
    // Destination is a symlink into the repository cache, without the extension.
    assert!(is_symlink(&fx.home_path(".gitconfig")));
    assert!(!fx.home_path(".gitconfig.tmpl").exists());
    let cache = cache_target(&fx, ".gitconfig");
    assert!(cache.starts_with(fx.repo.join(".rustowcache/rendered")), "{cache:?}");
    assert_eq!(
        read(&cache),
        "[user]\n    name = Example User\n[core]\n    editor = nvim\n"
    );
    assert_eq!(mode(&cache), 0o600);
    assert_eq!(mode(&fx.repo.join(".rustowcache")), 0o700);
    // A variable change re-renders the cache; the symlink stays valid.
    fx.write(".rustow.toml", &config("Other Name"));
    let out = fx.ok(&["template", "apply"]);
    assert!(out.stdout.contains("update"), "{out:?}");
    assert_eq!(cache_target(&fx, ".gitconfig"), cache);
    assert!(read(&fx.home_path(".gitconfig")).contains("name = Other Name"));
    // Unchanged render: nothing is rewritten.
    let (cache_time, link_time) = (mtime(&cache), mtime(&fx.home_path(".gitconfig")));
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = fx.ok(&["template", "apply"]);
    assert!(out.stdout.contains("0 changed"), "{out:?}");
    assert_eq!(
        (mtime(&cache), mtime(&fx.home_path(".gitconfig"))),
        (cache_time, link_time)
    );
}

#[test]
fn template_list_shows_templates() {
    let fx = templated();
    fx.write(".bashrc", "x");
    let out = fx.ok(&["template", "list"]);
    assert!(
        out.stdout.contains(".gitconfig.tmpl") && out.stdout.contains("template+link"),
        "{out:?}"
    );
    assert!(!out.stdout.contains(".bashrc"), "{out:?}");
}

#[test]
fn missing_var_is_error_naming_template() {
    let fx = Fixture::new();
    fx.write(".gitconfig.tmpl", "x\n{{ vars.nope }}\n");
    fx.write(".bashrc", "b");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains(".gitconfig.tmpl, line 2"), "{out:?}");
    assert!(
        !fx.home_path(".bashrc").exists(),
        "default policy: nothing changes when an item fails"
    );
    assert!(!fx.home_path(".gitconfig").exists());
}

#[test]
fn custom_extension() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[templates]\nextension = \".j2\"\n[vars]\nx = \"y\"\n",
    );
    fx.write(".profile.j2", "{{ vars.x }}\n");
    fx.write("keep.tmpl", "{{ literal }}");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".profile")), "y\n");
    assert_eq!(read(&fx.home_path("keep.tmpl")), "{{ literal }}");
}

#[test]
fn edits_through_the_symlink_are_detected_as_modified() {
    let fx = templated();
    fx.ok(&["apply"]);
    fs::write(cache_target(&fx, ".gitconfig"), "edited by hand\n").unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("cache entry"), "{out:?}");
    assert_eq!(read(&fx.home_path(".gitconfig")), "edited by hand\n");
    let diff = fx.run(&["diff"]);
    assert_eq!(diff.code, 1);
    assert!(diff.stdout.contains("-edited by hand"), "{diff:?}");
    fx.ok(&["apply", "--backup"]);
    assert!(read(&fx.home_path(".gitconfig")).contains("Example User"));
}

#[test]
fn failed_render_keeps_old_cache_and_dest() {
    let fx = templated();
    fx.ok(&["apply"]);
    let before = read(&fx.home_path(".gitconfig"));
    fx.write(".gitconfig.tmpl", "{{ vars.missing }}\n");
    assert_eq!(fx.run(&["apply", "--on-conflict", "skip"]).code, 4);
    assert_eq!(read(&fx.home_path(".gitconfig")), before);
}

#[test]
fn link_never_touches_templates() {
    let fx = templated();
    let out = fx.run(&["link", ".gitconfig.tmpl"]);
    assert_eq!(out.code, 2, "{out:?}");
    fx.ok(&["link"]);
    assert!(!fx.home_path(".gitconfig").exists());
    assert!(!fx.home_path(".gitconfig.tmpl").exists());
    assert!(!fx.repo.join(".rustowcache").exists());
}

#[test]
fn broken_managed_symlink_repaired() {
    // A plain source becomes a template: the old link now dangles, but it is
    // rustow's own, so it is replaced without --force.
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n[vars]\nv = \"new\"\n");
    fx.write(".profile", "old\n");
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join(".profile")).unwrap();
    fx.write(".profile.tmpl", "{{ vars.v }}\n");
    assert!(fs::metadata(fx.home_path(".profile")).is_err(), "link dangles now");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".profile")), "new\n");
}

#[test]
fn dry_run_creates_no_cache() {
    let fx = templated();
    let out = fx.ok(&["apply", "--dry-run"]);
    assert!(out.stdout.contains(".gitconfig"), "{out:?}");
    assert!(!fx.repo.join(".rustowcache").exists());
}

#[test]
fn cache_root_symlink_is_refused() {
    let fx = templated();
    let elsewhere = fx.base.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, fx.repo.join(".rustowcache")).unwrap();
    let out = fx.run(&["apply"]);
    assert_ne!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains(".rustowcache"), "{out:?}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(!Path::new(&fx.home_path(".gitconfig")).exists());
}

#[test]
fn force_recreates_a_removed_destination_over_a_tampered_cache_entry() {
    let fx = templated();
    fx.ok(&["apply"]);
    fs::write(cache_target(&fx, ".gitconfig"), "edited\n").unwrap();
    fs::remove_file(fx.home_path(".gitconfig")).unwrap();
    let out = fx.run(&["apply", "--force"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(read(&fx.home_path(".gitconfig")).contains("Example User"));
}
