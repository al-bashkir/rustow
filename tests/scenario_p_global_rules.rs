//! `[[global_rules]]`: per-host `vars` and `ignore` patterns.
//! `family = "unix"` is true on every test host; `os = "plan9"` never is.

mod common;

use common::*;

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

/// Deploys `.gitconfig` and `.config/foot/` with `config`, then adds a
/// matching rule that ignores `pattern`. The next runs report nothing for
/// the deployed `dest`, and `undeploy orphaned` keeps it.
fn ignored_after_deploy(fx: &Fixture, config: &str, pattern: &str, dest: &str) {
    fx.write("rustow.toml", config);
    fx.write(".gitconfig", "g\n");
    fx.write(".config/foot/foot.ini", "f\n");
    fx.write(".config/foot/themes/dark.ini", "d\n");
    fx.ok(&["apply"]);
    let content = read(&fx.home_path(dest));
    let rule = format!("[[global_rules]]\nfamily = \"unix\"\nignore = [\"{pattern}\"]\n");
    fx.write("rustow.toml", &format!("{config}{rule}"));
    let out = fx.ok(&["apply"]);
    assert!(!out.stdout.contains("orphaned"), "{out:?}");
    let st = fx.ok(&["status"]);
    assert!(!st.stdout.contains("ORPHANED"), "{st:?}");
    fx.ok(&["undeploy", "orphaned"]);
    assert_eq!(read(&fx.home_path(dest)), content, "kept");
}

#[test]
fn rule_ignore_after_deploy_leaves_the_file_and_its_record() {
    let fx = Fixture::new();
    ignored_after_deploy(&fx, "version = 1\n", "/.gitconfig", ".gitconfig");
    let st = fx.ok(&["status", "--ignored"]);
    assert!(
        st.stdout
            .lines()
            .any(|l| l.contains("IGNORED") && l.contains(".gitconfig")),
        "{st:?}"
    );
    fx.write("rustow.toml", "version = 1\n");
    // Without the record, the link would be UNMANAGED and status would exit 1.
    let st = fx.ok(&["status", "-v"]);
    assert!(st.stdout.contains("LINKED"), "{st:?}");
}

#[test]
fn rule_ignore_after_deploy_leaves_an_unfolded_folder() {
    let config = "version = 1\n[[file_rules]]\nsource = \".config/foot\"\nfold_directories = false\n";
    let fx = Fixture::new();
    ignored_after_deploy(&fx, config, ".config/foot/", ".config/foot/foot.ini");
    assert!(is_symlink(&fx.home_path(".config/foot/foot.ini")));
    // A path argument still selects the ignored destination.
    fx.ok(&["undeploy", ".config/foot"]);
    assert!(!fx.home_path(".config/foot/foot.ini").exists());
}

#[test]
fn rule_ignore_after_deploy_leaves_a_folded_folder() {
    let fx = Fixture::new();
    ignored_after_deploy(&fx, "version = 1\n", ".config/foot/", ".config/foot/foot.ini");
    assert!(is_symlink(&fx.home_path(".config/foot")));
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
