//! A write that fails part way leaves no partial file: not at the
//! destination, not as a temporary file next to it, not as a backup.

mod common;

use common::*;
use nix::sys::resource::Resource;
use std::fs;
use std::path::Path;

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
