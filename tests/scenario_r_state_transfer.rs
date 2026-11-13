//! `rustow state export|import`: move the registrations of `file add`
//! and copy-mode `link` to another machine.

mod common;

use common::*;
use serde_json::{Value, json};

fn machine() -> Fixture {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n[defaults]\nmode = \"copy\"\n");
    fx.write("a.conf", "a\n");
    fx.write(".bashrc", "b\n");
    fx.write(".vimrc", "v\n");
    fx
}

#[test]
fn export_then_import_on_another_machine() {
    let old = machine();
    old.ok(&["file", "add", "a.conf"]);
    old.ok(&["link", ".bashrc"]);
    let out = old.ok(&["state", "export"]);
    let exported: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        exported,
        json!({"version": 1, "files": ["a.conf"], "links": [".bashrc"]})
    );
    let file = old.base.join("regs.json");
    assert_eq!(old.run(&["state", "export", "-n", file.to_str().unwrap()]).code, 2);
    assert!(!file.exists());
    old.ok(&["state", "export", file.to_str().unwrap()]);
    assert_eq!(read(&file), out.stdout);

    let new = machine();
    new.ok(&["state", "import", "-n", file.to_str().unwrap()]);
    assert_eq!(new.state_json(), Value::Null, "a dry run imports nothing");
    let out = new.ok(&["state", "import", file.to_str().unwrap()]);
    assert!(
        out.stdout.contains("a.conf") && out.stdout.contains(".bashrc"),
        "{out:?}"
    );
    assert_eq!(new.state_json()["files"], json!(["a.conf"]));
    assert_eq!(new.state_json()["links"], json!([".bashrc"]));
    let again = new.ok(&["state", "import", file.to_str().unwrap()]);
    assert!(!again.stdout.contains("import "), "{again:?}");

    new.ok(&["apply"]);
    assert!(is_regular(&new.home_path("a.conf")));
    assert!(is_symlink(&new.home_path(".bashrc")));
    assert!(!new.home_path(".vimrc").exists());
}

#[test]
fn import_rejects_a_bad_file_and_changes_nothing() {
    let fx = machine();
    for (name, text, word) in [
        ("future.json", r#"{"version": 2, "files": []}"#, "version 2"),
        ("escape.json", r#"{"version": 1, "files": ["../x"]}"#, ".."),
        ("extra.json", r#"{"version": 1, "entries": {}}"#, "entries"),
        ("text.json", "not json", "text.json"),
    ] {
        let path = fx.base.join(name);
        write_file(&path, text);
        let out = fx.run(&["state", "import", path.to_str().unwrap()]);
        assert_eq!(out.code, 2, "{name}: {out:?}");
        assert!(out.stderr.contains(word), "{name}: {out:?}");
    }
    assert_eq!(fx.state_json(), Value::Null);
}

/// `import` checks each path as `file add` and `link <path>` do: a bad path
/// exits 2, names the path and the reason, and changes nothing.
#[test]
fn import_refuses_paths_that_file_add_and_link_refuse() {
    let fx = machine();
    fx.write(".t.tmpl", "t\n");
    fx.write(".config/d/x", "x\n");
    let none: &[&str] = &[];
    for (file, link, word) in [
        ("nonexist", none, "'nonexist' does not match any deployable source"),
        (".t.tmpl", none, "source '.t.tmpl': templates cannot be registered"),
        (".git/config", none, "'.git/config' is reserved"),
        (".rustow-tmp-a", none, "'.rustow-tmp-a' is reserved"),
        (".rustowcache/x", none, "'.rustowcache/x' is reserved"),
        (".config/d", none, "'.config/d' is a directory"),
        ("a.conf", &["nolink"], "'nolink' does not match any deployable source"),
        ("a.conf", &[".t.tmpl"], "'.t.tmpl' has no sources this command deploys"),
        ("a.conf", &[".config/d"], "'.config/d' is a directory"),
    ] {
        let path = fx.base.join("regs.json");
        let links: Vec<&str> = [".bashrc"].iter().chain(link).copied().collect();
        let regs = json!({"version": 1, "files": ["a.conf", file], "links": links});
        write_file(&path, &regs.to_string());
        let out = fx.run(&["state", "import", path.to_str().unwrap()]);
        assert_eq!(out.code, 2, "{regs}: {out:?}");
        assert!(out.stderr.contains(word), "{regs}: {out:?}");
        assert_eq!(fx.state_json(), Value::Null, "nothing changes: {regs}");
    }
}
