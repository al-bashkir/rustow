//! `[defaults] mode = "copy"`: `apply` makes no new symlink. It deploys
//! registered copies, templates as copies, and the links that the state
//! records (made by `rustow link <path>`). Other plain files wait.

mod common;

use common::*;
use std::fs;

const CONFIG: &str = "version = 1\n[defaults]\nmode = \"copy\"\n[vars]\nv = \"t\"\n\
                      [[files]]\nsource = \"copied.conf\"\n";

fn copy_mode() -> Fixture {
    let fx = Fixture::new();
    fx.write(".rustow.toml", CONFIG);
    fx.write(".bashrc", "b\n");
    fx.write(".vimrc", "v\n");
    fx.write("copied.conf", "c\n");
    fx.write(".gitconfig.tmpl", "{{ vars.v }}\n");
    fx
}

fn exists(fx: &Fixture, rel: &str) -> bool {
    fs::symlink_metadata(fx.home_path(rel)).is_ok()
}

#[test]
fn apply_deploys_copies_and_templates_but_makes_no_link() {
    let fx = copy_mode();
    let out = fx.ok(&["apply"]);
    assert!(!out.stderr.contains("warning"), "{out:?}");
    assert!(!exists(&fx, ".bashrc") && !exists(&fx, ".vimrc"), "{out:?}");
    assert!(is_regular(&fx.home_path("copied.conf")));
    assert!(is_regular(&fx.home_path(".gitconfig")), "templates copy");
    assert_eq!(read(&fx.home_path(".gitconfig")), "t\n");
}

#[test]
fn link_adds_one_file_and_apply_keeps_it() {
    let fx = copy_mode();
    fx.ok(&["link", ".bashrc"]);
    assert!(is_symlink(&fx.home_path(".bashrc")));
    assert!(!exists(&fx, ".vimrc"));

    fs::remove_file(fx.home_path(".bashrc")).unwrap();
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".bashrc")), "the state remembers the link");
    assert!(!exists(&fx, ".vimrc"));

    fx.ok(&["link"]);
    assert!(!exists(&fx, ".vimrc"), "`link` without a path adds nothing");

    fx.ok(&["unlink", ".bashrc"]);
    fx.ok(&["apply"]);
    assert!(!exists(&fx, ".bashrc"), "unlink forgets the link");
}

#[test]
fn a_path_argument_that_names_only_unlinked_files_is_a_usage_error() {
    let fx = copy_mode();
    let out = fx.run(&["apply", ".vimrc"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("rustow link .vimrc"), "{out:?}");
    assert!(!exists(&fx, ".vimrc"));
}

#[test]
fn a_linked_directory_folds_and_apply_keeps_the_fold() {
    let fx = copy_mode();
    fx.write(".config/nvim/init.lua", "i\n");
    fx.write(".config/nvim/lua/x.lua", "x\n");
    fx.ok(&["link", ".config/nvim"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")));
    fx.write(".config/nvim/lua/y.lua", "y\n");
    let out = fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")), "{out:?}");
}

#[test]
fn registering_a_linked_file_turns_it_into_a_copy() {
    let fx = copy_mode();
    fx.ok(&["link", ".bashrc"]);
    fx.ok(&["file", "add", ".bashrc"]);
    fx.ok(&["apply"]);
    assert!(is_regular(&fx.home_path(".bashrc")));
}

#[test]
fn an_unknown_mode_is_a_configuration_error() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n[defaults]\nmode = \"hardlink\"\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("defaults.mode"), "{out:?}");
}
