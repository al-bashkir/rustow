//! Folding by default, never directly under home, and `[[file_rules]]`.

mod common;

use common::*;
use std::fs;

fn is_dir(path: &std::path::Path) -> bool {
    fs::symlink_metadata(path).unwrap().is_dir()
}

#[test]
fn default_folds_below_top_level_but_never_a_direct_child_of_home() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n");
    fx.write(".config/nvim/init.lua", "i\n");
    fx.write(".ssh/config", "c\n");
    fx.ok(&["apply"]);
    assert!(is_dir(&fx.home_path(".config")), ".config is top-level");
    assert!(is_symlink(&fx.home_path(".config/nvim")), "folded one level down");
    assert!(is_dir(&fx.home_path(".ssh")), ".ssh is top-level");
    assert!(is_symlink(&fx.home_path(".ssh/config")));
}

#[test]
fn file_rule_keeps_its_directories_unfolded_and_a_sibling_subdir_folds() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \".config/a/b.txt\"\nfold_directories = false\n",
    );
    fx.write(".config/a/b.txt", "1\n");
    fx.write(".config/a/c/b.txt", "2\n");
    fs::create_dir_all(fx.home_path(".config")).unwrap();
    fx.ok(&["apply"]);
    assert!(is_dir(&fx.home_path(".config/a")));
    assert!(is_symlink(&fx.home_path(".config/a/b.txt")));
    assert!(is_symlink(&fx.home_path(".config/a/c")));
}

#[test]
fn directory_rule_false_with_deeper_rule_true() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \".config/nvim\"\nfold_directories = false\n\
         [[file_rules]]\nsource = \".config/nvim/lua\"\nfold_directories = true\n",
    );
    fx.write(".config/nvim/init.lua", "i\n");
    fx.write(".config/nvim/lua/x.lua", "x\n");
    fx.ok(&["apply"]);
    assert!(is_dir(&fx.home_path(".config/nvim")));
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
    assert!(is_symlink(&fx.home_path(".config/nvim/lua")));
}

#[test]
fn rules_can_fold_a_directory_when_the_default_is_false() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[defaults]\nfold_directories = false\n\
         [[file_rules]]\nsource = \".config/nvim\"\nfold_directories = true\n",
    );
    fx.write(".config/nvim/init.lua", "i\n");
    fx.write(".config/git/config", "g\n");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")));
    assert!(is_dir(&fx.home_path(".config/git")), "no rule: default false");
}

#[test]
fn new_false_rule_unfolds_an_existing_fold() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n");
    fx.write(".config/nvim/init.lua", "i\n");
    fx.write(".config/nvim/lua/x.lua", "x\n");
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")));
    fx.write(
        ".rustow.toml",
        "version = 1\n[[file_rules]]\nsource = \".config/nvim/init.lua\"\nfold_directories = false\n",
    );
    fx.ok(&["apply"]);
    assert!(is_dir(&fx.home_path(".config/nvim")));
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
    // The unfold gives every source its own link; `lua` exists from then on, so it never folds.
    assert!(is_symlink(&fx.home_path(".config/nvim/lua/x.lua")));
}

/// A fold of a direct child of home, recorded by an older rustow, no
/// longer qualifies: the next apply unfolds it.
#[test]
fn recorded_top_level_fold_is_unfolded_on_upgrade() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n");
    fx.write(".local/bin/x", "x\n");
    fs::create_dir_all(fx.home_path(".local")).unwrap();
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".local/bin")));

    // Turn the recorded `.local/bin` fold into a `.local` fold.
    let state_file = walkdir::WalkDir::new(&fx.state)
        .into_iter()
        .map(|e| e.unwrap().into_path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .expect("state file");
    let mut state: serde_json::Value = serde_json::from_str(&read(&state_file)).unwrap();
    let entries = state["entries"].as_object_mut().unwrap();
    let old_key = fx.home_path(".local/bin").display().to_string();
    let mut record = entries.remove(&old_key).expect("recorded fold");
    let target = fx.repo.join(".local");
    record["source"] = ".local".into();
    record["link_target"] = target.display().to_string().into();
    entries.insert(fx.home_path(".local").display().to_string(), record);
    fs::write(&state_file, serde_json::to_string(&state).unwrap()).unwrap();
    fs::remove_file(fx.home_path(".local/bin")).unwrap();
    fs::remove_dir(fx.home_path(".local")).unwrap();
    std::os::unix::fs::symlink(&target, fx.home_path(".local")).unwrap();

    fx.ok(&["apply"]);
    assert!(is_dir(&fx.home_path(".local")), "unfolded");
    assert!(is_symlink(&fx.home_path(".local/bin/x")));
    assert_eq!(read(&fx.home_path(".local/bin/x")), "x\n");
}
