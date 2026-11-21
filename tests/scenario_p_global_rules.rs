//! `[[global_rules]]`: per-host `vars` and `ignore` patterns.
//! `family = "unix"` is true on every test host; `os = "plan9"` never is.

mod common;

use common::*;
use std::fs;

const RULE1: &str = "[[global_rules]] #1 (family == \"unix\")";

#[test]
fn matching_rules_merge_vars_in_file_order() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        r#"version = 1
[vars]
user = "alice"
uid = 1000
[vars.git]
name = "A"
email = "a@home"

[[global_rules]]
family = "unix"
vars = { user = "asmith", git = { email = "a@work" } }

[[global_rules]]
os = "plan9"
vars = { uid = 9 }

[[global_rules]]
family = "unix"
vars = { user = "last" }
"#,
    );
    fx.write(
        ".t.tmpl",
        "{{ vars.user }} {{ vars.uid }} {{ vars.git.name }} {{ vars.git.email }}\n",
    );
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(".t")), "last 1000 A a@work\n");
}

#[test]
fn matching_rule_ignores_paths_quietly() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        r#"version = 1
[[files]]
source = ".gitconfig"

[[file_rules]]
source = ".config/foot"
fold_directories = false

[[global_rules]]
family = "unix"
ignore = [".config/foot/", "/.gitconfig"]

[[global_rules]]
os = "plan9"
ignore = [".bashrc"]
"#,
    );
    fx.write(".config/foot/foot.ini", "f\n");
    fx.write(".gitconfig", "g\n");
    fx.write(".bashrc", "b\n");
    let out = fx.ok(&["apply"]);
    assert!(!out.stderr.contains("warning"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!fx.home_path(".gitconfig").exists());
    assert!(!fx.home_path(".config/foot").exists());
    let st = fx.ok(&["status", "--ignored"]);
    for p in [".config/foot", ".gitconfig"] {
        assert!(
            st.stdout.lines().any(|l| l.contains("IGNORED") && l.contains(p)),
            "{p}: {st:?}"
        );
    }
    let arg = fx.run(&["apply", ".gitconfig"]);
    assert_eq!(arg.code, 2, "{arg:?}");
    assert!(
        arg.stderr.contains(&format!("'.gitconfig' is ignored by {RULE1}")),
        "{arg:?}"
    );
}

/// `config` plus a rule that matches every test host and ignores `patterns`.
fn with_rule(config: &str, patterns: &str) -> String {
    format!("{config}[[global_rules]]\nfamily = \"unix\"\nignore = [{patterns}]\n")
}

/// Deploys `.gitconfig` and `.config/foot/` with `config`, then adds a
/// matching rule that ignores `pattern`. `status` shows the deployed `dest`
/// as UNWANTED, `apply` removes it, and without the rule `apply` deploys
/// it again.
fn removed_after_ignore(fx: &Fixture, config: &str, pattern: &str, dest: &str) {
    fx.write("rustow.toml", config);
    fx.write(".gitconfig", "g\n");
    fx.write(".config/foot/foot.ini", "f\n");
    fx.write(".config/foot/themes/dark.ini", "d\n");
    fx.ok(&["apply"]);
    let content = read(&fx.home_path(dest));
    fx.write("rustow.toml", &with_rule(config, &format!("\"{pattern}\"")));
    let st = fx.run(&["status"]);
    assert_eq!(st.code, 1, "{st:?}");
    let detail = format!("ignored by {RULE1}; `rustow apply` removes it");
    assert!(st.stdout.contains("UNWANTED") && st.stdout.contains(&detail), "{st:?}");
    assert!(!st.stdout.contains("ORPHANED"), "{st:?}");
    let dry = fx.ok(&["apply", "--dry-run"]);
    assert!(dry.stdout.contains("remove ~/"), "{dry:?}");
    assert_eq!(read(&fx.home_path(dest)), content, "a dry run removes nothing");
    let out = fx.ok(&["apply"]);
    assert!(
        out.stdout.contains("remove ~/") && !out.stdout.contains("orphaned"),
        "{out:?}"
    );
    assert!(fx.home_path(dest).symlink_metadata().is_err(), "removed: {out:?}");
    fx.ok(&["status"]);
    let again = fx.ok(&["apply"]);
    assert!(!again.stdout.contains("remove"), "{again:?}");
    fx.write("rustow.toml", config);
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.home_path(dest)), content, "deployed again");
}

#[test]
fn rule_ignore_after_deploy_removes_the_file() {
    let fx = Fixture::new();
    removed_after_ignore(&fx, "version = 1\n", "/.gitconfig", ".gitconfig");
}

#[test]
fn rule_ignore_after_deploy_removes_an_unfolded_folder() {
    let config = "version = 1\n[[file_rules]]\nsource = \".config/foot\"\nfold_directories = false\n";
    let fx = Fixture::new();
    removed_after_ignore(&fx, config, ".config/foot/", ".config/foot/themes/dark.ini");
    assert!(is_symlink(&fx.home_path(".config/foot/foot.ini")));
}

#[test]
fn rule_ignore_after_deploy_removes_a_folded_folder() {
    let fx = Fixture::new();
    removed_after_ignore(&fx, "version = 1\n", ".config/foot/", ".config/foot/foot.ini");
    assert!(is_symlink(&fx.home_path(".config/foot")));
}

#[test]
fn unwanted_status_in_json() {
    let fx = Fixture::new();
    fx.write(".gitconfig", "g\n");
    fx.ok(&["apply"]);
    fx.write("rustow.toml", &with_rule("version = 1\n", "\"/.gitconfig\""));
    let st = fx.run(&["status", "--json"]);
    assert_eq!(st.code, 1, "{st:?}");
    let v: serde_json::Value = serde_json::from_str(&st.stdout).unwrap();
    let item = &v["items"][0];
    assert_eq!(item["status"], "UNWANTED", "{v}");
    assert_eq!(item["source"], ".gitconfig", "{v}");
}

#[test]
fn rule_ignore_keeps_a_changed_copy() {
    let config = "version = 1\n[[files]]\nsource = \".gitconfig\"\n";
    let fx = Fixture::new();
    fx.write("rustow.toml", config);
    fx.write(".gitconfig", "g\n");
    fx.ok(&["apply"]);
    fs::write(fx.home_path(".gitconfig"), "mine\n").unwrap();
    fx.write(".bashrc", "b\n");
    fx.write("rustow.toml", &with_rule(config, "\"/.gitconfig\""));
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("conflict ~/.gitconfig"), "{out:?}");
    assert_eq!(read(&fx.home_path(".gitconfig")), "mine\n", "kept");
    assert!(is_symlink(&fx.home_path(".bashrc")), "the skip policy deploys the rest");
    assert_eq!(fx.run(&["status"]).code, 1, "still UNWANTED");
    assert_eq!(
        fx.run(&["apply", "--force"]).code,
        3,
        "a changed destination always stays"
    );
    assert_eq!(read(&fx.home_path(".gitconfig")), "mine\n");
}

#[test]
fn rule_ignore_removes_only_what_the_command_selects() {
    let fx = Fixture::new();
    fx.write(".gitconfig", "g\n");
    fx.write(".config/foot/foot.ini", "f\n");
    fx.write(".config/git/config", "c\n");
    fx.ok(&["apply"]);
    fx.write(
        "rustow.toml",
        &with_rule("version = 1\n", "\"/.gitconfig\", \".config/foot/\""),
    );
    fx.ok(&["file", "apply"]);
    assert!(
        is_symlink(&fx.home_path(".gitconfig")),
        "file apply does not remove links"
    );
    fx.ok(&["apply", ".config"]);
    assert!(!fx.home_path(".config/foot").exists());
    assert!(is_symlink(&fx.home_path(".gitconfig")), "not under .config");
    fx.ok(&["link"]);
    assert!(!fx.home_path(".gitconfig").exists());
}

#[test]
fn bad_rules_are_configuration_errors_without_values() {
    let fx = Fixture::new();
    for (rule, want) in [
        (
            "ignore = [\"x\"]\n",
            "[[global_rules]] #1: set at least one condition: os, arch, family, hostname, username",
        ),
        ("family = \"unix\"\nhost = \"S3CRET\"\n", "key global_rules[0].host"),
        ("family = \"unix\"\nvars = \"S3CRET\"\n", "key global_rules[0].vars"),
        (
            "family = \"unix\"\nignore = [\"{a\"]\n",
            "[[global_rules]] #1: ignore: error parsing glob",
        ),
        (
            "os = \"plan9\"\nignore = [\"{a\"]\n",
            "[[global_rules]] #1: ignore: error parsing glob",
        ),
    ] {
        fx.write("rustow.toml", &format!("version = 1\n[[global_rules]]\n{rule}"));
        let out = fx.run(&["apply"]);
        assert_eq!(out.code, 2, "{rule}: {out:?}");
        assert!(out.stderr.contains(want), "{rule}: {out:?}");
        assert!(!out.stderr.contains("S3CRET"), "{rule}: {out:?}");
    }
}
