//! Built-in exclusions plus `.rustowignore` (gitignore syntax).
//!
//! Reserved paths are excluded first and cannot be re-included by a
//! negation pattern. Patterns match repository-relative paths, so
//! `/rustowroot/etc/foo` ignores the system file `/etc/foo`, while an
//! unanchored `*.bak` also matches inside `rustowroot/`.

use std::path::Path;

use ::ignore::gitignore::{Gitignore, GitignoreBuilder};
use nix::fcntl::OFlag;

use crate::config::{CONFIG_FILE, GlobalRule, read_config_file};
use crate::error::{Error, Result};
use crate::paths::RelPath;
use crate::template::Host;

pub const IGNORE_FILE: &str = ".rustowignore";
pub const DEFAULT_CACHE_DIR: &str = ".rustowcache";
pub const ROOT_DIR: &str = "rustowroot";

/// The most ignore patterns that rustow reads into its matchers. Each
/// costs memory (a glob like `a/**/b*.y` about 8 KiB), so the 16 MiB of a
/// file at the size limit could need gigabytes.
pub const MAX_RULES: usize = 10_000;

/// Fails when `count` patterns of `what` are more than `MAX_RULES`.
fn within_limit(what: &str, count: usize) -> Result<()> {
    match count > MAX_RULES {
        true => Err(Error::Config(format!(
            "{what}: {count} patterns; rustow reads at most {MAX_RULES}"
        ))),
        false => Ok(()),
    }
}

/// The text of `.rustowignore` at `file` (empty when it is missing), with at
/// most `MAX_RULES` patterns; comments and blank lines do not count.
fn read_ignore_file(file: &Path) -> Result<String> {
    let text = read_config_file(file, OFlag::empty())?.unwrap_or_default();
    let patterns = text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
    within_limit(IGNORE_FILE, patterns.count())?;
    Ok(text)
}

/// Root-level files that are never sources.
const RESERVED_ROOT_FILES: [&str; 3] = [".gitignore", crate::config::CONFIG_FILE, IGNORE_FILE];

pub struct Ignore {
    matcher: Gitignore,
    /// Configured cache and state directories inside the repository.
    internal: Vec<RelPath>,
    /// The `[[global_rules]]` that match this host: name and patterns.
    /// Each works alone, so a rule can only add ignored paths.
    rules: Vec<(String, Gitignore)>,
}

impl Ignore {
    /// Reads `<repo>/.rustowignore` when present. `internal` lists the
    /// configured cache/state directories that lie inside the repository.
    pub fn load(repo: &Path, internal: Vec<RelPath>) -> Result<Ignore> {
        let mut builder = GitignoreBuilder::new(repo);
        let file = repo.join(IGNORE_FILE);
        let text = read_ignore_file(&file)?;
        let errors: Vec<String> = text
            .lines()
            .enumerate()
            .filter_map(|(i, line)| {
                // Git ignores a byte order mark at the start of the file.
                let line = if i == 0 {
                    line.trim_start_matches('\u{feff}')
                } else {
                    line
                };
                let added = builder.add_line(Some(file.clone()), line);
                added.err().map(|e| format!("line {}: {e}", i + 1))
            })
            .collect();
        if !errors.is_empty() {
            return Err(Error::Config(format!("{IGNORE_FILE}: {}", errors.join("; "))));
        }
        let matcher = builder
            .build()
            .map_err(|e| Error::Config(format!("{IGNORE_FILE}: {e}")))?;
        Ok(Ignore {
            matcher,
            internal,
            rules: Vec::new(),
        })
    }

    /// Adds the `ignore` patterns of each rule that matches `host`. The
    /// patterns of every rule must be valid, also on other hosts.
    pub fn with_rules(mut self, repo: &Path, rules: &[GlobalRule], host: &Host) -> Result<Ignore> {
        let count = rules.iter().map(|r| r.ignore.len()).sum();
        within_limit(&format!("{CONFIG_FILE}: [[global_rules]] ignore"), count)?;
        for rule in rules {
            let bad = |e: ::ignore::Error| {
                Error::Config(format!("{CONFIG_FILE}: [[global_rules]] #{}: ignore: {e}", rule.number))
            };
            let mut builder = GitignoreBuilder::new(repo);
            for pattern in &rule.ignore {
                builder.add_line(None, pattern).map_err(bad)?;
            }
            let matcher = builder.build().map_err(bad)?;
            if rule.matches(host) {
                self.rules.push((rule.to_string(), matcher));
            }
        }
        Ok(self)
    }

    /// The name of the matching `[[global_rules]]` entry that ignores the path.
    pub fn ignoring_rule(&self, path: &RelPath, is_dir: bool) -> Option<&str> {
        self.rules
            .iter()
            .find(|(_, m)| excludes(m, path, is_dir))
            .map(|(name, _)| name.as_str())
    }

    /// Built-in exclusion. The `rustowroot` directory is traversed, but a
    /// file with that name is reserved.
    pub fn is_reserved(&self, path: &RelPath, is_dir: bool) -> bool {
        let p = path.as_str();
        p.split('/')
            .any(|c| c == ".git" || c.starts_with(crate::fsops::TEMP_PREFIX))
            || RESERVED_ROOT_FILES.contains(&p)
            || (p == ROOT_DIR && !is_dir)
            || p == DEFAULT_CACHE_DIR
            || p.starts_with(".rustowcache/")
            || self.internal.iter().any(|dir| path.starts_with(dir))
    }

    /// True when a configured cache or state directory lies at or below
    /// `dir`, whether or not it exists yet.
    pub fn holds_internal(&self, dir: &RelPath) -> bool {
        self.internal.iter().any(|d| d.starts_with(dir))
    }

    /// Reserved, or excluded by `.rustowignore` or a matching
    /// `[[global_rules]]` entry (the path or any ancestor). An ignored
    /// ancestor directory cannot be re-included, as in Git.
    pub fn is_ignored(&self, path: &RelPath, is_dir: bool) -> bool {
        self.is_reserved(path, is_dir)
            || excludes(&self.matcher, path, is_dir)
            || self.ignoring_rule(path, is_dir).is_some()
    }
}

/// Git semantics: an excluded ancestor directory excludes the whole
/// subtree, whatever later negations say about its children.
pub fn excludes(matcher: &Gitignore, path: &RelPath, is_dir: bool) -> bool {
    path.ancestors().rev().any(|dir| matcher.matched(dir, true).is_ignore())
        || matcher.matched(path.as_path(), is_dir).is_ignore()
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
            "!.git\n!.git/**\n!.gitignore\n!rustow.toml\n!.rustowignore\n!.rustowcache/\n!.rustowcache/**\n",
        );
        for (p, dir) in [
            (".git", true),
            (".git/config", false),
            (".gitignore", false),
            ("rustow.toml", false),
            (".rustowignore", false),
            (".rustowcache", true),
            (".rustowcache/rendered/x", false),
            ("rustowroot", false),
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
        assert!(!ig.is_reserved(&rel("rustowroot/etc/x"), false));
        assert!(!ig.is_reserved(&rel("rustowroot"), true));
    }

    #[test]
    fn rustow_temp_files_reserved_at_any_depth() {
        let (_d, ig) = with_rules("!.rustow-tmp-*\n!**/.rustow-tmp-*\n");
        let temp = format!("{}Ab12Cd", crate::fsops::TEMP_PREFIX);
        for p in [temp.clone(), format!(".config/app/{temp}"), format!("{temp}/x")] {
            assert!(ig.is_reserved(&rel(&p), false), "{p}");
            assert!(ig.is_ignored(&rel(&p), false), "{p}");
        }
        assert!(!ig.is_reserved(&rel(".config/rustow-tmp-x"), false));
        assert!(!ig.is_reserved(&rel(".rustow-notes"), false));
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
        let (_d, ig) = with_rules("/rustowroot/etc/secret.conf\n*.bak\n/etc/\n");
        assert!(ig.is_ignored(&rel("rustowroot/etc/secret.conf"), false));
        assert!(ig.is_ignored(&rel("rustowroot/etc/x.bak"), false));
        // An anchored "/etc/" matches the repository's etc/, not the system one.
        assert!(!ig.is_ignored(&rel("rustowroot/etc/other.conf"), false));
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
