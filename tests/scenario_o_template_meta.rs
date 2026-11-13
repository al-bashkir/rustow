//! Templates with a meta block: settings, conditions, rendering, status.

mod common;

use common::*;

#[test]
fn meta_settings_copy_with_mode_and_block_is_not_in_output() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(
        ".x.conf.tmpl",
        "{%- meta.mode = \"0600\" %}\n{%- meta.family == \"unix\" %}\nhello\n",
    );
    fx.ok(&["apply"]);
    let p = fx.home_path(".x.conf");
    assert!(is_regular(&p));
    assert_eq!(mode(&p) & 0o777, 0o600);
    assert_eq!(read(&p), "hello\n");
}

#[test]
fn false_condition_is_skipped_quietly() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(".x.tmpl", "{%- meta.os == \"plan9\" %}\nx\n");
    let out = fx.ok(&["apply"]);
    assert!(!out.stderr.contains("warning"), "{out:?}");
    assert!(!fx.home_path(".x").exists());
    let st = fx.run(&["status"]);
    assert_eq!(st.code, 0, "{st:?}");
    assert!(
        st.stdout.contains("SKIPPED") && st.stdout.contains("meta.os == \"plan9\" is false here"),
        "{st:?}"
    );
    let selected = fx.ok(&["apply", ".x.tmpl"]);
    assert!(!selected.stderr.contains("warning"), "{selected:?}");
    assert!(!fx.ok(&["check"]).stderr.contains("warning"));
}

#[test]
fn condition_false_after_deploy_is_orphaned() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(".x.tmpl", "x\n");
    fx.ok(&["apply"]);
    fx.write(".x.tmpl", "{%- meta.os == \"plan9\" %}\nx\n");
    let st = fx.run(&["status"]);
    assert_eq!(st.code, 1, "{st:?}");
    assert!(
        st.stdout.contains("ORPHANED") && st.stdout.contains("meta.os == \"plan9\" is false here"),
        "{st:?}"
    );
    assert!(
        !st.stdout.contains("SKIPPED"),
        "one row, not ORPHANED + SKIPPED: {st:?}"
    );
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("meta.os == \"plan9\" is false here"), "{out:?}");
    assert_eq!(read(&fx.home_path(".x")), "x\n", "nothing removed");
}

#[test]
fn render_error_names_the_original_line() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(
        ".x.tmpl",
        "{%- meta.mode = \"0600\" %}\n{%- meta.family == \"unix\" %}\n{{ missing }}\n",
    );
    let out = fx.run(&["check"]);
    assert!(out.stdout.contains("template .x.tmpl, line 3"), "{out:?}");
    let applied = fx.run(&["apply"]);
    assert!(applied.stdout.contains("template .x.tmpl, line 3"), "{applied:?}");
}

#[test]
fn meta_lines_in_an_encrypted_template_fail_to_render() {
    let fx = Fixture::new();
    let key = fx.age_key("main");
    fx.age_config(&[&key], &[&key], "");
    fx.age_encrypt(".x.tmpl.age", "{%- meta.mode = \"0600\" %}\nx\n", &[&key]);
    let out = fx.run(&["apply"]);
    assert_ne!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains(".x.tmpl.age"), "{out:?}");
    assert!(!fx.home_path(".x").exists());
}

#[test]
fn check_reports_an_unknown_meta_owner_or_group_as_apply_does() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(".u.tmpl", "{%- meta.owner = \"no-such-user-rustow\" %}\nu\n");
    fx.write(".g.tmpl", "{%- meta.group = \"no-such-group-rustow\" %}\ng\n");
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 1, "{out:?}");
    for want in [
        "problem: .u.tmpl: template meta block: unknown user 'no-such-user-rustow'",
        "problem: .g.tmpl: template meta block: unknown group 'no-such-group-rustow'",
    ] {
        assert!(out.stdout.contains(want), "{want}: {out:?}");
    }
    let applied = fx.run(&["apply"]);
    assert_eq!(applied.code, 4, "{applied:?}");
    assert!(
        applied.stdout.contains("unknown user 'no-such-user-rustow'"),
        "{applied:?}"
    );
}

#[test]
fn check_fails_on_a_broken_meta_block_but_apply_only_warns() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[template_rules]]\nsource = \"b.tmpl\"\nmode = \"0600\"\n",
    );
    fx.write(".x.tmpl", "{%- meta.user = \"root\" %}\nx\n");
    fx.write("b.tmpl", "{%- meta.uid = 0 %}\nb\n");
    let out = fx.run(&["check"]);
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout
            .contains("problem: .x.tmpl: template meta block, line 1: unknown meta key 'meta.user'"),
        "{out:?}"
    );
    assert!(out.stdout.contains("problem: b.tmpl: set metadata either"), "{out:?}");
    let applied = fx.run(&["apply"]);
    assert_eq!(applied.code, 0, "{applied:?}");
    assert!(
        applied.stderr.contains("warning: .x.tmpl: template meta block"),
        "{applied:?}"
    );
}
