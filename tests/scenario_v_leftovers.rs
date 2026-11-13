//! A write that fails part way leaves no partial file: not at the
//! destination, not as a temporary file next to it, not as a backup.

mod common;

use common::*;
use nix::sys::resource::Resource;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The regular files at or below `dir`.
fn files_below(dir: &Path) -> Vec<std::path::PathBuf> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => fs::read_dir(dir)
            .unwrap()
            .flat_map(|e| files_below(&e.unwrap().path()))
            .collect(),
        Ok(m) if m.is_file() => vec![dir.to_path_buf()],
        _ => Vec::new(),
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A file larger than the file size limit (`ulimit -f`) fails its item:
/// the write gets "File too large" instead of a signal that kills rustow.
#[test]
fn the_file_size_limit_fails_the_item_and_leaves_no_temp() {
    let fx = Fixture::new();
    fx.write(".big", &"x".repeat(64 << 10));
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \".big\"\n");
    let out = fx.run_limit(Resource::RLIMIT_FSIZE, 32 << 10, &["apply"], &[]);
    assert_eq!(out.code, 4, "{out:?}");
    let item = out.stdout.lines().find(|l| l.contains("~/.big")).unwrap_or_default();
    assert!(item.contains("File too large"), "{out:?}");
    assert_eq!(names(&fx.home), ["dotfiles"], "no copy and no temporary file");
}

/// The journal of temporary files after a first apply, and the temporary
/// file `dir/.rustow-tmp-dead`, as a run that was killed leaves them.
fn killed_run(fx: &Fixture, dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    fx.write(".a", "a\n");
    fx.ok(&["apply"]);
    let lock = fs::read_dir(fx.state.join("rustow"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "lock"))
        .unwrap();
    let journal = lock.with_extension("pending");
    assert!(!journal.exists(), "a run that ends removes its journal");
    let temp = dir.join(".rustow-tmp-dead");
    fs::write(&temp, "half").unwrap();
    fs::write(&journal, format!("{}\n", serde_json::json!(temp))).unwrap();
    (journal, temp)
}

/// The next run that takes the lock removes a temporary file that a
/// killed run left, and says so; `doctor` reports it before that.
#[test]
fn the_next_run_removes_a_temporary_file_that_a_killed_run_left() {
    let fx = Fixture::new();
    let (journal, temp) = killed_run(&fx, &fx.home);
    let doctor = fx.run(&["doctor"]);
    let line = doctor
        .stdout
        .lines()
        .find(|l| l.contains("temp file"))
        .unwrap_or_default();
    assert!(
        line.starts_with(" warn") && line.contains(temp.to_str().unwrap()),
        "{doctor:?}"
    );
    let dry = fx.ok(&["apply", "-n"]);
    assert!(temp.exists(), "a dry run changes nothing: {dry:?}");
    let out = fx.ok(&["apply"]);
    let note = format!("note: removed {}", temp.display());
    assert!(out.stderr.contains(&note), "{out:?}");
    assert!(!temp.exists() && !journal.exists());
    let after = fx.ok(&["doctor"]);
    assert!(
        after
            .stdout
            .contains("   ok  temp files: none left by an interrupted run"),
        "{after:?}"
    );
}

/// A leftover that the user cannot remove (here in a directory without
/// write access; in practice a file of the sudo helper) stays, with its
/// journal line, and rustow prints the command that removes it.
#[test]
fn a_leftover_the_user_cannot_remove_gets_the_command_to_remove_it() {
    let fx = Fixture::new();
    let ro = fx.base.join("ro dir");
    fs::create_dir(&ro).unwrap();
    let (journal, temp) = killed_run(&fx, &ro);
    let set_mode = |mode| fs::set_permissions(&ro, fs::Permissions::from_mode(mode)).unwrap();
    set_mode(0o555);
    let command = format!("sudo rm -- '{}'", temp.display());
    let doctor = fx.run(&["doctor"]);
    let out = fx.run(&["apply"]);
    let again = read(&journal);
    set_mode(0o755);
    assert!(doctor.stdout.contains(&command), "{doctor:?}");
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stderr.contains(&command), "{out:?}");
    assert!(temp.exists(), "{out:?}");
    assert_eq!(again, format!("{}\n", serde_json::json!(temp)), "the journal keeps it");
}

/// Each temporary file of the sudo helper is in the journal before the
/// helper starts; rustow names it.
#[test]
fn the_temporary_files_of_the_helper_are_journaled_first() {
    let fx = Fixture::new();
    fx.write(
        "rustow.toml",
        "version = 1\n[[files]]\nsource = \"rustowroot/etc/app.conf\"\n",
    );
    fx.write("rustowroot/etc/app.conf", "a\n");
    let etc = fx.root.join("etc");
    fs::create_dir(&etc).unwrap();
    let copy = fx.base.join("journal-copy");
    let sudo = write_file(
        &fx.base.join("fake-sudo"),
        &format!(
            "#!/bin/sh\ncat \"$XDG_STATE_HOME\"/rustow/*.pending > '{}'\nshift\nexec \"$@\"\n",
            copy.display()
        ),
    );
    fs::set_permissions(&sudo, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o555)).unwrap();
    let out = fx.run_env(&["apply"], &[("RUSTOW_SUDO", sudo.to_str().unwrap())]);
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).unwrap();
    // The helper runs as the user here, so it cannot write etc: the write
    // fails, which does not matter for the journal.
    assert_eq!(out.code, 4, "{out:?}");
    let journaled = read(&copy);
    let temp = format!("\"{}/.rustow-tmp-", etc.display());
    assert!(journaled.lines().any(|l| l.starts_with(&temp)), "{journaled}");
    assert_eq!(names(&etc), Vec::<String>::new(), "no temporary file stays");
    assert!(
        !fx.state
            .join("rustow")
            .read_dir()
            .unwrap()
            .any(|e| { e.unwrap().path().extension().is_some_and(|x| x == "pending") })
    );
}

/// A backup into another filesystem is a copy. When the copy fails (here
/// at the file size limit), no partial backup stays, and the original
/// stays in place.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_backup_across_filesystems_leaves_no_partial_backup() {
    use std::os::unix::fs::MetadataExt;
    let fx = Fixture::new();
    let shm = Path::new("/dev/shm");
    let device = |p: &Path| fs::metadata(p).map(|m| m.dev()).ok();
    if !shm.is_dir() || device(shm) == device(&fx.base) {
        eprintln!("SKIP: needs /dev/shm on another filesystem than {}", fx.base.display());
        return;
    }
    let state = tempfile::tempdir_in(shm).unwrap();
    fx.write(".conf", "new\n");
    fx.write("rustow.toml", "version = 1\n[[files]]\nsource = \".conf\"\n");
    let old = "old\n".repeat(16 << 10);
    fx.write_home(".conf", &old);
    let env = [("XDG_STATE_HOME", state.path().to_str().unwrap())];
    let out = fx.run_limit(Resource::RLIMIT_FSIZE, 32 << 10, &["apply", "--backup"], &env);
    assert_eq!(out.code, 4, "{out:?}");
    assert!(out.stdout.contains("File too large"), "{out:?}");
    assert_eq!(read(&fx.home_path(".conf")), old, "the original stays");
    let backups = state.path().join("rustow/backups");
    assert_eq!(
        files_below(&backups),
        Vec::<std::path::PathBuf>::new(),
        "no partial backup"
    );
}
