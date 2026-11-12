//! Spec §17.2 Scenario H — conflict detection (symlink part).

mod common;

use common::*;
use std::fs;

fn two() -> Fixture {
    let fx = Fixture::new();
    fx.write(".bashrc", "repo bash\n");
    fx.write(".zshrc", "repo zsh\n");
    fx
}

#[test]
fn unmanaged_file_conflict_exit_3_nothing_changed() {
    let fx = two();
    fx.write_home(".bashrc", "mine\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(
        out.stdout.contains("conflict") && out.stdout.contains(".bashrc"),
        "{out:?}"
    );
    assert!(out.stdout.contains("not managed by rustow"), "{out:?}");
    assert!(out.stdout.contains("conflicts found, nothing written"), "{out:?}");
    assert!(!out.stdout.contains("dry run"), "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "mine\n");
    assert!(!fx.home_path(".zshrc").exists(), "abort policy changes nothing");
}

#[test]
fn skip_policy_deploys_the_rest() {
    let fx = two();
    fx.write_home(".bashrc", "mine\n");
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "mine\n");
    assert!(is_symlink(&fx.home_path(".zshrc")));
}

#[test]
fn unmanaged_symlink_conflict() {
    let fx = two();
    std::os::unix::fs::symlink("/somewhere/else", fx.home_path(".bashrc")).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert_eq!(link_text(&fx.home_path(".bashrc")).to_str(), Some("/somewhere/else"));
}

#[test]
fn foreign_broken_symlink_conflict_then_force() {
    let fx = two();
    std::os::unix::fs::symlink("dotfiles/gone", fx.home_path(".bashrc")).unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("broken symlink"), "{out:?}");
    fx.ok(&["apply", "--force"]);
    assert_eq!(read(&fx.home_path(".bashrc")), "repo bash\n");
}

#[test]
fn adopts_existing_correct_symlink() {
    let fx = two();
    std::os::unix::fs::symlink("dotfiles/.bashrc", fx.home_path(".bashrc")).unwrap();
    let out = fx.ok(&["apply"]);
    assert!(out.stdout.contains("adopt"), "{out:?}");
    // Adopted links are managed: unlink removes them.
    fx.ok(&["unlink", ".bashrc"]);
    assert!(!is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn backup_moves_conflicting_file() {
    let fx = two();
    fx.write_home(".bashrc", "mine\n");
    let out = fx.ok(&["apply", "--backup"]);
    assert!(out.stdout.contains("backup"), "{out:?}");
    assert_eq!(read(&fx.home_path(".bashrc")), "repo bash\n");
    let backups: Vec<_> = walk(&fx.state.join("rustow/backups"));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(read(&backups[0]), "mine\n");
    assert!(backups[0].ends_with("home/.bashrc"), "{backups:?}");
}

#[test]
fn force_never_replaces_dir() {
    let fx = two();
    fs::create_dir_all(fx.home_path(".bashrc/keep")).unwrap();
    let out = fx.run(&["apply", "--force"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(fx.home_path(".bashrc/keep").is_dir());
}

#[test]
fn unlink_removes_only_ours() {
    let fx = two();
    fx.ok(&["apply"]);
    fs::remove_file(fx.home_path(".zshrc")).unwrap();
    fx.write_home(".zshrc", "user replaced it\n");
    let out = fx.run(&["unlink"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!fx.home_path(".bashrc").exists());
    assert_eq!(read(&fx.home_path(".zshrc")), "user replaced it\n");
    assert!(fx.repo.join(".bashrc").exists(), "sources are never removed");
}

#[test]
fn unlink_removes_created_empty_dirs_only() {
    let fx = Fixture::new();
    fx.write(".config/nvim/init.lua", "x");
    fx.write(".config/git/ignore", "y");
    fx.ok(&["apply"]);
    fx.write_home(".config/git/user-file", "keep");
    fx.ok(&["unlink"]);
    assert!(!fx.home_path(".config/nvim").exists(), "created and now empty");
    assert!(fx.home_path(".config/git/user-file").exists());
    assert!(fx.home_path(".config").is_dir());
}

#[test]
fn unlink_dry_run_changes_nothing() {
    let fx = two();
    fx.ok(&["apply"]);
    let out = fx.ok(&["unlink", "--dry-run"]);
    assert!(out.stdout.contains("remove"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn unlink_handles_removed_source() {
    let fx = two();
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join(".zshrc")).unwrap();
    fx.ok(&["unlink", ".zshrc"]);
    assert!(!is_symlink(&fx.home_path(".zshrc")));
    assert!(is_symlink(&fx.home_path(".bashrc")));
}

#[test]
fn missing_parent_without_create_directories_conflicts() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", "version = 1\n[defaults]\ncreate_directories = false\n");
    fx.write(".config/x", "x");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!fx.home_path(".config").exists());
}

#[test]
fn second_run_is_locked_out_while_first_holds_lock() {
    let fx = two();
    fs::create_dir_all(fx.state.join("rustow")).unwrap();
    // Hold the lock of whatever repo id rustow computes by locking every *.lock it creates.
    fx.ok(&["apply"]);
    let lock = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "lock"))
        .expect("lock file");
    let held = fs::OpenOptions::new().write(true).open(&lock).unwrap();
    held.try_lock().unwrap();
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stderr.contains("another rustow"), "{out:?}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Spec §14.6: a home link is trust-checked like a system link. With a
/// root-owned target (as under `sudo -H`), a link into the user's
/// repository is refused. A dry run never writes, so /usr is safe here.
#[test]
fn home_link_into_a_repository_the_target_owner_does_not_own_is_untrusted() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("SKIP: root owns the repository too");
        return;
    }
    let fx = Fixture::new();
    fx.write(".bashrc", "x");
    fx.write(".t.tmpl", "y");
    let out = fx.run(&["--target", "/usr", "apply", "--dry-run"]);
    assert_eq!(out.code, 3, "{out:?}");
    let line = |src: &str| {
        out.stdout
            .lines()
            .find(|l| l.contains(src))
            .unwrap_or_else(|| panic!("no line for {src}: {out:?}"))
            .to_owned()
    };
    let plain = line("<- .bashrc");
    assert!(plain.contains("conflict") && plain.contains("untrusted"), "{plain}");
    assert!(
        plain.contains("repository source") && plain.contains("rustow file add"),
        "{plain}"
    );
    assert!(!plain.contains("cache") && !plain.contains("template_rules"), "{plain}");
    let cached = line("<- .t.tmpl");
    assert!(
        cached.contains("untrusted") && cached.contains("cache entry"),
        "{cached}"
    );
}
