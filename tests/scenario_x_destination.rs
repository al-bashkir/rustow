//! `destination`: a source in one place of the repository deploys to
//! another path below the target root of its scope.

mod common;

use common::*;

#[test]
fn a_file_rule_moves_a_link() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \"scripts/hello.sh\"\ndestination = \".local/bin/hello\"\n",
    );
    let source = fx.write("scripts/hello.sh", "echo hi\n");
    fx.ok(&["apply"]);
    let dest = fx.home_path(".local/bin/hello");
    assert!(is_symlink(&dest));
    assert_eq!(
        std::fs::canonicalize(&dest).unwrap(),
        std::fs::canonicalize(&source).unwrap()
    );
    assert!(!fx.home_path("scripts").exists(), "nothing at the source path");
}

#[test]
fn a_files_entry_moves_a_copy_with_its_metadata() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"conf/app.conf\"\nmode = \"0600\"\ndestination = \".config/app/app.conf\"\n",
    );
    fx.write("conf/app.conf", "a = 1\n");
    fx.ok(&["apply"]);
    let dest = fx.home_path(".config/app/app.conf");
    assert!(is_regular(&dest));
    assert_eq!(mode(&dest), 0o600);
    assert_eq!(read(&dest), "a = 1\n");
    assert!(!fx.home_path("conf").exists());
}

/// A template rule with only a destination keeps the template a link.
#[test]
fn a_template_rule_moves_a_template() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[vars]\nname = \"x\"\n[[template_rules]]\nsource = \"t/greet.tmpl\"\ndestination = \".greet\"\n",
    );
    fx.write("t/greet.tmpl", "hello {{ vars.name }}\n");
    fx.ok(&["apply"]);
    let dest = fx.home_path(".greet");
    assert!(is_symlink(&dest), "template+link");
    assert_eq!(read(&dest), "hello x\n");
}

#[test]
fn a_system_file_moves_below_the_root_target() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/shared/app.conf\"\ndestination = \"etc/app.conf\"\n",
    );
    fx.write("rustowroot/shared/app.conf", "s\n");
    fx.ok(&["apply"]);
    assert_eq!(read(&fx.root_path("etc/app.conf")), "s\n");
    assert!(!fx.root_path("shared").exists());
}

#[test]
fn a_destination_that_another_source_has_is_a_collision() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \"b\"\ndestination = \"a\"\n",
    );
    fx.write("a", "1\n");
    fx.write("b", "2\n");
    let out = fx.run(&["apply"]);
    assert_ne!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains("map to the same destination 'a'"), "{out:?}");
    assert!(!fx.home_path("a").exists());
}

/// `destination` names one file; on a directory it warns and does nothing.
#[test]
fn a_destination_on_a_directory_warns() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \"dir\"\ndestination = \"elsewhere\"\n",
    );
    fx.write("dir/f", "f\n");
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr
            .contains("warning: dir: rustow.toml [[file_rules]] entry is a directory, not a file"),
        "{out:?}"
    );
    assert!(is_symlink(&fx.home_path("dir/f")));
    assert!(!fx.home_path("elsewhere").exists());
}

/// A fold would show the repository directory, where the file has its
/// source name: a directory that holds a moved source never folds.
#[test]
fn a_moved_source_keeps_its_directory_unfolded() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \".config/app/run.sh\"\ndestination = \".config/app/run\"\n",
    );
    fx.write(".config/app/run.sh", "r\n");
    fx.write(".config/app/conf", "c\n");
    fx.ok(&["apply"]);
    assert!(!is_symlink(&fx.home_path(".config/app")), "not folded");
    assert!(is_symlink(&fx.home_path(".config/app/run")));
    assert!(is_symlink(&fx.home_path(".config/app/conf")));
    assert!(!fx.home_path(".config/app/run.sh").exists());
}

/// A fold of the directory that a source moves into would write that
/// file into the repository.
#[test]
fn a_directory_that_a_source_moves_into_never_folds() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \"scripts/run\"\ndestination = \".config/app/run\"\n",
    );
    fx.write("scripts/run", "r\n");
    fx.write(".config/app/conf", "c\n");
    fx.ok(&["apply"]);
    assert!(!is_symlink(&fx.home_path(".config/app")), "not folded");
    assert!(is_symlink(&fx.home_path(".config/app/run")));
    assert!(
        !fx.repo.join(".config/app/run").exists(),
        "nothing written into the repository"
    );
}

/// A fold from an earlier run unfolds when a source moves into it.
#[test]
fn a_source_that_moves_into_a_fold_unfolds_it() {
    let fx = Fixture::new();
    fx.write("rustow.toml", "version = 1\n");
    fx.write(".config/app/conf", "c\n");
    fx.write("scripts/run", "r\n");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".config/app")), "folded");
    fx.write(
        "rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \"scripts/run\"\ndestination = \".config/app/run\"\n",
    );
    fx.ok(&["apply"]);
    assert!(!is_symlink(&fx.home_path(".config/app")), "unfolded");
    assert!(is_symlink(&fx.home_path(".config/app/conf")));
    assert!(is_symlink(&fx.home_path(".config/app/run")));
    assert!(
        !fx.repo.join(".config/app/run").exists(),
        "nothing written into the repository"
    );
}
