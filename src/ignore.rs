//! Built-in exclusions plus `.rustowignore` (gitignore syntax).
//!
//! Reserved paths are excluded first and cannot be re-included by a
//! negation pattern. Patterns match repository-relative paths, so
//! `/.rustowroot/etc/foo` ignores the system file `/etc/foo`, while an
//! unanchored `*.bak` also matches inside `.rustowroot/`.

use std::path::Path;

use ::ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::error::{Error, Result};
use crate::paths::RelPath;

pub const IGNORE_FILE: &str = ".rustowignore";
pub const DEFAULT_CACHE_DIR: &str = ".rustowcache";
pub const ROOT_DIR: &str = ".rustowroot";

/// Root-level files that are never sources.
const RESERVED_ROOT_FILES: [&str; 3] = [".gitignore", crate::config::CONFIG_FILE, IGNORE_FILE];

pub struct Ignore {
    matcher: Gitignore,
    /// Configured cache and state directories inside the repository.
    internal: Vec<RelPath>,
}

impl Ignore {
    /// Reads `<repo>/.rustowignore` when present. `internal` lists the
    /// configured cache/state directories that lie inside the repository.
    pub fn load(repo: &Path, internal: Vec<RelPath>) -> Result<Ignore> {
        let mut builder = GitignoreBuilder::new(repo);
        let file = repo.join(IGNORE_FILE);
        if file.exists()
            && let Some(err) = builder.add(&file)
        {
            return Err(Error::Config(format!("{IGNORE_FILE}: {err}")));
        }
        let matcher = builder
            .build()
            .map_err(|e| Error::Config(format!("{IGNORE_FILE}: {e}")))?;
        Ok(Ignore { matcher, internal })
    }

    /// Built-in exclusion. The `.rustowroot` directory is traversed, but a
    /// file with that name is reserved.
    pub fn is_reserved(&self, path: &RelPath, is_dir: bool) -> bool {
        let p = path.as_str();
        p.split('/').any(|c| c == ".git")
            || RESERVED_ROOT_FILES.contains(&p)
            || (p == ROOT_DIR && !is_dir)
            || p == DEFAULT_CACHE_DIR
            || p.starts_with(".rustowcache/")
            || self.internal.iter().any(|dir| path.starts_with(dir))
    }

    /// Reserved, or excluded by `.rustowignore` (the path or any ancestor).
    /// An ignored ancestor directory cannot be re-included, as in Git.
    pub fn is_ignored(&self, path: &RelPath, is_dir: bool) -> bool {
        if self.is_reserved(path, is_dir) {
            return true;
        }
        let mut ancestors = Vec::new();
        let mut cur = path.parent();
        while let Some(p) = cur {
            cur = p.parent();
            ancestors.push(p);
        }
        ancestors
            .iter()
            .rev()
            .any(|dir| self.matcher.matched(dir.as_path(), true).is_ignore())
            || self.matcher.matched(path.as_path(), is_dir).is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    fn with_rules(rules: &str) -> (tempfile::TempDir, Ignore) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(IGNORE_FILE), rules).unwrap();
        let ig = Ignore::load(dir.path(), vec![]).unwrap();
        (dir, ig)
    }

    #[test]
    fn reserved_always_excluded_even_with_negation() {
        let (_d, ig) = with_rules(
            "!.git\n!.git/**\n!.gitignore\n!.rustow.toml\n!.rustowignore\n!.rustowcache/\n!.rustowcache/**\n",
        );
        for (p, dir) in [
            (".git", true),
            (".git/config", false),
            (".gitignore", false),
            (".rustow.toml", false),
            (".rustowignore", false),
            (".rustowcache", true),
            (".rustowcache/rendered/x", false),
            (".rustowroot", false),
        ] {
            assert!(ig.is_reserved(&rel(p), dir), "{p}");
            assert!(ig.is_ignored(&rel(p), dir), "{p}");
        }
    }

    #[test]
    fn cache_subtree_excluded_without_ignore_files() {
        let dir = tempfile::tempdir().unwrap();
        let ig = Ignore::load(dir.path(), vec![]).unwrap();
        assert!(ig.is_ignored(&rel(".rustowcache/decrypted/a/b/.ssh/x"), false));
        assert!(!ig.is_ignored(&rel(".bashrc"), false));
    }

    #[test]
    fn configured_internal_dirs_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let ig = Ignore::load(dir.path(), vec![rel("var/cache")]).unwrap();
        assert!(ig.is_reserved(&rel("var/cache/x"), false));
        assert!(ig.is_reserved(&rel("var/cache"), true));
        assert!(!ig.is_reserved(&rel("var/cachex"), false));
        // The default cache stays excluded when another one is configured.
        assert!(ig.is_reserved(&rel(".rustowcache/x"), false));
    }

    #[test]
    fn nested_dot_git_excluded_nested_gitignore_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ig = Ignore::load(dir.path(), vec![]).unwrap();
        assert!(ig.is_reserved(&rel(".config/nvim/pack/x/.git"), true));
        assert!(ig.is_reserved(&rel(".config/nvim/pack/x/.git/HEAD"), false));
        assert!(!ig.is_reserved(&rel(".config/foo/.gitignore"), false));
        assert!(!ig.is_reserved(&rel(".rustowroot/etc/x"), false));
        assert!(!ig.is_reserved(&rel(".rustowroot"), true));
    }

    #[test]
    fn negation_reincludes_ordinary_file() {
        let (_d, ig) = with_rules("*.conf\n!important.conf\n");
        assert!(ig.is_ignored(&rel("a/x.conf"), false));
        assert!(!ig.is_ignored(&rel("a/important.conf"), false));
    }

    #[test]
    fn ignored_dir_cannot_be_reentered_by_child_negation() {
        let (_d, ig) = with_rules("private/\n!private/keep\n");
        assert!(ig.is_ignored(&rel("private"), true));
        assert!(ig.is_ignored(&rel("private/keep"), false));
        assert!(ig.is_ignored(&rel("x/private/deep/file"), false));
    }

    #[test]
    fn ignore_applies_under_rustowroot() {
        let (_d, ig) = with_rules("/.rustowroot/etc/secret.conf\n*.bak\n/etc/\n");
        assert!(ig.is_ignored(&rel(".rustowroot/etc/secret.conf"), false));
        assert!(ig.is_ignored(&rel(".rustowroot/etc/x.bak"), false));
        // An anchored "/etc/" matches the repository's etc/, not the system one.
        assert!(!ig.is_ignored(&rel(".rustowroot/etc/other.conf"), false));
        assert!(ig.is_ignored(&rel("etc/x"), false));
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let (_d, ig) = with_rules("# *.lua\n\n*.tmp\n");
        assert!(!ig.is_ignored(&rel("init.lua"), false));
        assert!(ig.is_ignored(&rel("x.tmp"), false));
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(IGNORE_FILE), "{a,b\n").unwrap();
        let err = Ignore::load(dir.path(), vec![]).err().unwrap().to_string();
        assert!(err.contains(IGNORE_FILE), "{err}");
    }
}
