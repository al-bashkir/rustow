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

/// A skipped source is not a link source: a directory that holds one
/// never folds, so the fold cannot expose it.
#[test]
fn directory_with_a_skipped_source_never_folds() {
    let fx = nvim(FOLD);
    fx.write(".config/nvim/n.age.gpg", "x");
    let out = fx.ok(&["apply"]);
    assert!(
        out.stderr
            .contains("warning: .config/nvim/n.age.gpg: nested encryption suffixes"),
        "{out:?}"
    );
    let dir = fx.home_path(".config/nvim");
    assert!(fs::symlink_metadata(&dir).unwrap().is_dir(), "never folded: {out:?}");
    assert!(is_symlink(&dir.join("init.lua")));
    assert!(!dir.join("n.age.gpg").exists());
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

/// `diff --json` describes an unfold and a collision in text: neither is binary.
#[test]
fn diff_json_shows_unfold_and_collision_as_text() {
    let fx = nvim(&format!("{FOLD}[vars]\nx = \"y\"\n"));
    fx.ok(&["apply"]);
    fx.write(".config/nvim/local.lua.tmpl", "{{ vars.x }}\n");
    fx.write("c", "c\n");
    fx.write("c.tmpl", "t\n");
    let out = fx.run(&["diff", "--json"]);
    assert_eq!(out.code, 1, "{out:?}");
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let items = v["items"].as_array().unwrap();
    for (source, text) in [
        (".config/nvim", "would become a real directory"),
        ("c", "same destination"),
    ] {
        let item = items.iter().find(|i| i["source"] == source);
        let item = item.unwrap_or_else(|| panic!("{source}: {v}"));
        assert!(item["diff"].as_str().unwrap().contains(text), "{source}: {v}");
        assert_eq!(item["binary"], false, "{source}: {v}");
    }
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

/// Review C1: a folded link replaced by a real directory with user files
/// must never lose those files, whatever the conflict policy.
#[test]
fn user_directory_in_place_of_folded_link_keeps_its_files() {
    for policy in ["skip", "backup"] {
        let fx = nvim(FOLD);
        fx.ok(&["apply"]);
        assert!(is_symlink(&fx.home_path(".config/nvim")));
        fs::remove_file(fx.home_path(".config/nvim")).unwrap();
        fx.write_home(".config/nvim/init.lua", "MY LOCAL EDITS\n");
        let out = fx.run(&["apply", "--on-conflict", policy]);
        assert!(
            !out.stdout.contains("create ~/.config/nvim/init.lua"),
            "{policy}: {out:?}"
        );
        match policy {
            "skip" => {
                assert_eq!(out.code, 3, "{out:?}");
                assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "MY LOCAL EDITS\n");
            }
            _ => {
                assert!(out.stdout.contains("backup saved"), "{out:?}");
                let backup = walk_files(&fx.state.join("rustow/backups"));
                assert_eq!(backup.len(), 1, "{backup:?}");
                assert_eq!(read(&backup[0]), "MY LOCAL EDITS\n");
            }
        }
    }
}

#[test]
fn blocked_unfold_blocks_files_below_it() {
    let fx = nvim(FOLD);
    fx.write(".rustowignore", "*.log\n");
    fx.ok(&["apply"]);
    fs::write(fx.home_path(".config/nvim/app.log"), "log").unwrap();
    fx.write(".config/nvim/new.lua", "new\n");
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(!out.stdout.contains("create ~/.config/nvim/"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim")), "the folded link stays");
}

fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_files(&p))
        } else {
            out.push(p)
        }
    }
    out
}

/// Audit F1: an unfold that does not deploy again every source below the
/// folded link would drop the others from view. It waits for `apply <dir>`.
#[test]
fn unfold_waits_until_every_source_below_it_is_deployed() {
    let cases: [&[&str]; 2] = [&["template", "apply"], &["apply", ".config/nvim/local.lua.tmpl"]];
    for args in cases {
        let fx = nvim(&format!("{FOLD}[vars]\nx = \"y\"\n"));
        fx.ok(&["apply"]);
        fx.write(".config/nvim/local.lua.tmpl", "{{ vars.x }}\n");
        let out = fx.run(args);
        assert_eq!(out.code, 3, "{args:?}: {out:?}");
        assert!(out.stdout.contains("rustow apply .config/nvim"), "{args:?}: {out:?}");
        assert!(
            is_symlink(&fx.home_path(".config/nvim")),
            "{args:?}: the folded link stays"
        );
        assert_eq!(read(&fx.home_path(".config/nvim/lua/plugins.lua")), "plugins\n");
        fx.ok(&["apply", ".config/nvim"]);
        assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "init\n");
        assert_eq!(read(&fx.home_path(".config/nvim/lua/plugins.lua")), "plugins\n");
        assert_eq!(read(&fx.home_path(".config/nvim/local.lua")), "y\n");
    }
}

/// The fold check does not follow a symlink below the directory: an
/// ignored symlink to an empty directory still blocks the fold, which
/// would expose it.
#[test]
fn ignored_symlink_to_a_directory_blocks_the_fold() {
    let fx = nvim(FOLD);
    fx.write(".rustowignore", ".config/nvim/shared\n");
    let empty = fx.base.join("empty");
    fs::create_dir(&empty).unwrap();
    std::os::unix::fs::symlink(&empty, fx.repo.join(".config/nvim/shared")).unwrap();
    fx.ok(&["apply"]);
    let dir = fx.home_path(".config/nvim");
    assert!(fs::symlink_metadata(&dir).unwrap().is_dir(), "not folded");
    assert!(is_symlink(&dir.join("init.lua")));
    assert!(
        fs::symlink_metadata(dir.join("shared")).is_err(),
        "the ignored link stays out"
    );
}

/// A held unfold stops only the sources below the folded link; the other
/// sources of the same run still deploy.
#[test]
fn held_unfold_leaves_other_sources_deployable() {
    let fx = nvim(&format!("{FOLD}[vars]\nx = \"y\"\n"));
    fx.ok(&["apply"]);
    fx.write(".config/nvim/local.lua.tmpl", "{{ vars.x }}\n");
    fx.write(".gitconfig.tmpl", "{{ vars.x }}\n");
    let out = fx.run(&["template", "apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("rustow apply .config/nvim"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim")), "{out:?}");
    assert_eq!(read(&fx.home_path(".gitconfig")), "y\n", "{out:?}");
}

#[test]
fn link_does_not_unfold_around_a_newly_registered_copy() {
    let fx = nvim(FOLD);
    fx.ok(&["apply"]);
    fx.ok(&["file", "add", ".config/nvim/init.lua"]);
    let out = fx.run(&["link"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("rustow apply .config/nvim"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim")));
    assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "init\n");
}

#[test]
fn unfold_is_a_conflict_when_an_item_below_it_is_not_deployed() {
    for policy in [&["--on-conflict", "skip"][..], &["--force"][..]] {
        let fx = nvim(FOLD);
        fx.write(".config/nvim/x", "x\n");
        fx.ok(&["apply"]);
        // A collision: both sources map to ~/.config/nvim/x, so neither deploys.
        fx.write(".config/nvim/x.tmpl", "t\n");
        let out = fx.run(&[&["apply"][..], policy].concat());
        assert_eq!(out.code, 3, "{policy:?}: {out:?}");
        assert!(is_symlink(&fx.home_path(".config/nvim")), "{policy:?}: {out:?}");
        assert_eq!(read(&fx.home_path(".config/nvim/x")), "x\n");
        assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "init\n");
    }
}

/// Audit F2: a directory that holds the in-repository cache or state
/// directory never folds; the fold would expose rendered plaintext.
#[test]
fn directory_holding_an_internal_directory_never_folds() {
    for (setting, init) in [("cache_dir", true), ("cache_dir", false), ("state_dir", false)] {
        let fx = Fixture::new();
        fx.write(".rustow.toml", &format!("{FOLD}{setting} = \".local/rc\"\n"));
        fx.write(".local/share/x", "x\n");
        fx.write(".gitconfig.tmpl", "g\n");
        if init {
            fx.ok(&["init"]);
        }
        fx.ok(&["apply"]);
        let case = format!("{setting}, init {init}");
        assert!(!is_symlink(&fx.home_path(".local")), "{case}");
        assert!(!fx.home_path(".local/rc").exists(), "{case}: stays hidden");
        assert_eq!(read(&fx.home_path(".local/share/x")), "x\n");
        assert!(fx.ok(&["apply"]).stdout.contains("0 changed"), "{case}");
    }
}

#[test]
fn unlink_forgets_a_fold_record_whose_link_the_user_replaced() {
    let fx = nvim(FOLD);
    fx.ok(&["apply"]);
    fs::remove_file(fx.home_path(".config/nvim")).unwrap();
    fx.write_home(".config/nvim/init.lua", "mine\n");
    let out = fx.run(&["unlink", ".config/nvim"]);
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains("forget"), "{out:?}");
    assert_eq!(read(&fx.home_path(".config/nvim/init.lua")), "mine\n");
    let again = fx.run(&["unlink", ".config/nvim"]);
    assert_eq!(again.code, 2, "record is gone: {again:?}");
}

/// The folded link is still in place, but its parent now resolves through
/// an unsafe symlink: the unfold is a conflict and nothing changes there.
#[test]
fn unfold_below_an_unsafe_parent_symlink_is_a_conflict() {
    use std::os::unix::fs::PermissionsExt;
    let fx = nvim(&format!("{FOLD}[vars]\nx = \"y\"\n"));
    fx.ok(&["apply"]);
    let open = fx.base.join("open");
    fs::rename(fx.home_path(".config"), &open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    std::os::unix::fs::symlink(&open, fx.home_path(".config")).unwrap();
    let link_before = fs::read_link(open.join("nvim")).unwrap();
    fx.write(".config/nvim/local.lua.tmpl", "{{ vars.x }}\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    let fold_line = out.stdout.lines().find(|l| l.contains("<- .config/nvim (link)"));
    assert!(
        fold_line.is_some_and(|l| l.contains("parent directory is blocked")),
        "{out:?}"
    );
    assert_eq!(fs::read_link(open.join("nvim")).unwrap(), link_before);
    assert!(!open.join("nvim/local.lua").exists());
}

/// Audit F3: a directory with a destination collision on itself or below
/// it never folds; every colliding source stays blocked, as without folding.
#[test]
fn collision_on_or_below_a_directory_blocks_its_fold() {
    for (file, template) in [("d/a", "d.tmpl"), ("d/e/a", "d/e.tmpl")] {
        let fx = Fixture::new();
        fx.write(".rustow.toml", FOLD);
        fx.write(file, "a\n");
        fx.write(template, "t\n");
        let out = fx.run(&["apply", "--on-conflict", "skip"]);
        assert_eq!(out.code, 3, "{file}: {out:?}");
        assert!(!is_symlink(&fx.home_path("d")), "{file}: {out:?}");
        assert!(!fx.home_path(file).exists(), "{file}: {out:?}");
    }
}

/// Audit F4: a selective run folds a new directory only when it selects
/// every source below it; a fold of ~/.config would expose b/y.
#[test]
fn selective_apply_folds_only_fully_selected_directories() {
    let fx = Fixture::new();
    fx.write(".rustow.toml", FOLD);
    fx.write(".config/a/x", "x\n");
    fx.write(".config/b/y", "y\n");
    let out = fx.ok(&["apply", ".config/a/x"]);
    assert!(!is_symlink(&fx.home_path(".config")), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/a")), "{out:?}");
    assert!(!fx.home_path(".config/b").exists(), "{out:?}");
}

/// Only a home collision at or below a directory blocks its fold: not one
/// elsewhere in the home target, and not one in the root target at the
/// same relative path.
#[test]
fn collisions_elsewhere_do_not_block_a_home_fold() {
    let fx = nvim(&format!("{FOLD}[[files]]\nsource = \".rustowroot/.config/nvim/x\"\n"));
    fx.write("c", "c\n");
    fx.write("c.tmpl", "t\n");
    fx.write(".rustowroot/.config/nvim/x", "x\n");
    fx.write(".rustowroot/.config/nvim/x.tmpl", "t\n");
    let out = fx.run(&["apply", "--on-conflict", "skip"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim")), "{out:?}");
}

/// A recorded fold unfolds only in a run that selects a source below it.
#[test]
fn selective_run_elsewhere_keeps_a_recorded_fold() {
    let fx = nvim(FOLD);
    fx.ok(&["apply"]);
    fx.write(".rustow.toml", "version = 1\n");
    fx.write(".bashrc", "b\n");
    let out = fx.ok(&["apply", ".bashrc"]);
    assert!(is_symlink(&fx.home_path(".config/nvim")), "{out:?}");
    assert!(is_symlink(&fx.home_path(".bashrc")), "{out:?}");
}

/// A plain file link is not a fold record: when its source becomes a
/// directory, the link is not unfolded.
#[test]
fn file_link_whose_source_became_a_directory_is_not_unfolded() {
    let fx = Fixture::new();
    fx.write("d", "file\n");
    fx.ok(&["apply"]);
    fs::remove_file(fx.repo.join("d")).unwrap();
    fx.write("d/x", "x\n");
    let out = fx.run(&["apply"]);
    assert_eq!(out.code, 3, "{out:?}");
    assert!(out.stdout.contains("inside the repository"), "{out:?}");
    assert!(is_symlink(&fx.home_path("d")), "{out:?}");
}

#[test]
fn selective_apply_keeps_an_existing_fold() {
    let fx = nvim(FOLD);
    fx.ok(&["apply"]);
    let out = fx.ok(&["apply", ".config/nvim/init.lua"]);
    assert!(out.stdout.contains("0 changed"), "{out:?}");
    assert!(is_symlink(&fx.home_path(".config/nvim")));
}
