//! Spec §17.2 Scenario F — mixed directories, folding and unfolding.

mod common;

use common::*;
use std::fs;

const FOLD: &str = "version = 1\n[defaults]\nfold_directories = true\n";

fn nvim(config: &str) -> Fixture {
    let fx = Fixture::new();
    fx.write(".rustow.toml", config);
    fx.write(".config/nvim/init.lua", "init\n");
    fx.write(".config/nvim/lua/plugins.lua", "plugins\n");
    fs::create_dir_all(fx.home_path(".config")).unwrap();
    fx
}

#[test]
fn mixed_directory_deploys_each_mode_and_never_folds() {
    let fx = Fixture::new();
    fx.write(
        ".rustow.toml",
        "version = 1\n[defaults]\nfold_directories = true\n[vars]\nv = \"r\"\n[[files]]\nsource = \".config/app/copied.conf\"\n",
    );
    fx.write(".rustowignore", "*.swp\n");
    fx.write(".config/app/plain.conf", "plain\n");
    fx.write(".config/app/copied.conf", "copied\n");
    fx.write(".config/app/rendered.conf.tmpl", "{{ vars.v }}\n");
    fx.write(".config/app/.plain.conf.swp", "junk");
    fs::create_dir_all(fx.home_path(".config")).unwrap();
    fx.ok(&["apply"]);
    let dir = fx.home_path(".config/app");
    assert!(fs::symlink_metadata(&dir).unwrap().is_dir(), "never folded");
    assert!(is_symlink(&dir.join("plain.conf")));
    assert!(is_regular(&dir.join("copied.conf")));
    assert!(is_symlink(&dir.join("rendered.conf")));
    assert_eq!(read(&dir.join("rendered.conf")), "r\n");
    assert!(!dir.join("rendered.conf.tmpl").exists());
    assert!(!dir.join(".plain.conf.swp").exists(), "ignored files stay out");
}

#[test]
fn fold_off_by_default() {
    let fx = nvim("version = 1\n");
    fx.ok(&["apply"]);
    assert!(fs::symlink_metadata(fx.home_path(".config/nvim")).unwrap().is_dir());
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
}

#[test]
fn fold_pure_dir() {
    let fx = nvim(FOLD);
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("~/.config/nvim "), "{out:?}");
    let dir = fx.home_path(".config/nvim");
    assert!(is_symlink(&dir), "a directory with only plain files is folded");
    assert_eq!(read(&dir.join("lua/plugins.lua")), "plugins\n");
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
    assert!(fx.ok(&["link"]).stdout.contains("0 changed"));
}

#[test]
fn no_fold_into_existing_directory() {
    let fx = nvim(FOLD);
    fs::create_dir_all(fx.home_path(".config/nvim")).unwrap();
    fx.ok(&["apply"]);
    assert!(fs::symlink_metadata(fx.home_path(".config/nvim")).unwrap().is_dir());
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
}

#[test]
fn no_fold_with_registered_file() {
    let fx = nvim(&format!("{FOLD}[[files]]\nsource = \".config/nvim/init.lua\"\n"));
    fx.ok(&["apply"]);
    assert!(fs::symlink_metadata(fx.home_path(".config/nvim")).unwrap().is_dir());
    assert!(is_regular(&fx.home_path(".config/nvim/init.lua")));
    assert!(
        fs::symlink_metadata(fx.home_path(".config/nvim/lua")).unwrap().is_dir()
            || is_symlink(&fx.home_path(".config/nvim/lua"))
    );
}

#[test]
fn unfold_when_template_added() {
    let fx = nvim(&format!("{FOLD}[vars]\nx = \"y\"\n"));
    fx.ok(&["apply"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")));
    fx.write(".config/nvim/local.lua.tmpl", "{{ vars.x }}\n");
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("unfold"), "{out:?}");
    let dir = fx.home_path(".config/nvim");
    assert!(fs::symlink_metadata(&dir).unwrap().is_dir());
    assert!(is_symlink(&dir.join("init.lua")));
    assert_eq!(read(&dir.join("lua/plugins.lua")), "plugins\n");
    assert_eq!(read(&dir.join("local.lua")), "y\n");
    assert!(!dir.join("local.lua.tmpl").exists());
    assert!(fx.ok(&["apply"]).stdout.contains("0 changed"));
}

#[test]
fn unfold_when_folding_is_turned_off() {
    let fx = nvim(FOLD);
    fx.ok(&["apply"]);
    fx.write(".rustow.toml", "version = 1\n");
    fx.ok(&["apply"]);
    assert!(fs::symlink_metadata(fx.home_path(".config/nvim")).unwrap().is_dir());
    assert!(is_symlink(&fx.home_path(".config/nvim/init.lua")));
}

#[test]
fn unfold_conflict_when_unrelated_files_would_disappear() {
    let fx = nvim(FOLD);
    fx.write(".rustowignore", "*.log\n");
    fx.ok(&["apply"]);
    // An application writes through the folded link: the file lands in the repository.
    fs::write(fx.home_path(".config/nvim/app.log"), "log").unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("app.log"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim")), "nothing changes");
    assert_eq!(read(&fx.repo.join(".config/nvim/app.log")), "log");
}

#[test]
fn unlink_removes_folded_dir_link() {
    let fx = nvim(FOLD);
    fx.ok(&["apply"]);
    fx.ok(&["unlink"]);
    assert!(!fx.home_path(".config/nvim").exists());
    assert!(fx.repo.join(".config/nvim/init.lua").exists());
}

#[test]
fn destination_resolving_into_repository_is_blocked() {
    let fx = Fixture::new();
    fx.write(".config/nvim/init.lua", "precious\n");
    std::os::unix::fs::symlink(fx.repo.join(".config"), fx.home_path(".config")).unwrap();
    for args in [&["apply"][..], &["apply", "--force"][..]] {
        let out = fx.run(args);
        assert_eq!(out.code, 3, "{out:?}");
        assert!(out.stdout.contains("inside the repository"), "{out:?}");
    }
    assert!(is_regular(&fx.repo.join(".config/nvim/init.lua")));
    assert_eq!(read(&fx.repo.join(".config/nvim/init.lua")), "precious\n");
}
