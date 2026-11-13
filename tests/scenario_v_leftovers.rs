//! A write that fails part way leaves no partial file: not at the
//! destination, not as a temporary file next to it, not as a backup.

mod common;

use common::*;
use nix::sys::resource::Resource;
use std::fs;
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
