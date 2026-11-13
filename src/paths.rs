//! Repository-relative paths, scopes and symlink text.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// A normalized repository-relative path: `/`-separated, UTF-8, at least one
/// component, and no `.`, `..` or empty components.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelPath(String);

impl RelPath {
    /// Parses user or configuration input. Rejects absolute paths, `..`,
    /// NUL bytes and empty input; collapses `.` and repeated `/`.
    pub fn parse(input: &str) -> Result<RelPath> {
        let invalid = |why: &str| Error::Usage(format!("invalid path '{input}': {why}"));
        if input.contains('\0') {
            return Err(invalid("contains a NUL byte"));
        }
        if input.starts_with('/') {
            return Err(invalid("must be relative to the repository root"));
        }
        let mut parts = Vec::new();
        for part in input.split('/') {
            match part {
                "" | "." => {}
                ".." => return Err(invalid("'..' is not allowed")),
                other => parts.push(other),
            }
        }
        if parts.is_empty() {
            return Err(invalid("empty path"));
        }
        Ok(RelPath(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    pub fn parent(&self) -> Option<RelPath> {
        self.0.rsplit_once('/').map(|(parent, _)| RelPath(parent.to_owned()))
    }

    /// The directories above this path, the nearest first. They borrow
    /// the text: copies would cost the square of the depth.
    pub fn ancestors(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.0.rmatch_indices('/').map(|(end, _)| &self.0[..end])
    }

    pub fn join(&self, child: &RelPath) -> RelPath {
        RelPath(format!("{}/{}", self.0, child.0))
    }

    /// Component-wise prefix test: `a/b` starts with `a` but not with `a/b2`.
    pub fn starts_with(&self, prefix: &RelPath) -> bool {
        self.0 == prefix.0 || self.strip_prefix(prefix).is_some()
    }

    /// The rest of the path below `prefix`, or `None` when it is not a strict prefix.
    pub fn strip_prefix(&self, prefix: &RelPath) -> Option<RelPath> {
        let rest = self.0.strip_prefix(&prefix.0)?.strip_prefix('/')?;
        Some(RelPath(rest.to_owned()))
    }

    /// The same path with its last component replaced. `name` must be one
    /// non-empty component.
    pub fn with_file_name(&self, name: &str) -> RelPath {
        debug_assert!(!name.is_empty() && !name.contains('/'));
        match self.0.rsplit_once('/') {
            Some((parent, _)) => RelPath(format!("{parent}/{name}")),
            None => RelPath(name.to_owned()),
        }
    }
}

impl serde::Serialize for RelPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for RelPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        RelPath::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Sets and maps of paths answer for the text of an ancestor (`ancestors`).
impl std::borrow::Borrow<str> for RelPath {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a source deploys: the home target, or the root target for `rustowroot/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Home,
    Root,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Home => "home",
            Scope::Root => "root",
        }
    }
}

/// Expands `~` and `~/...` against `home`. Other input is returned unchanged.
pub fn expand_tilde(input: &str, home: &Path) -> PathBuf {
    match input.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(input),
    }
}

/// Resolves `.` and `..` without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// True when the symlink texts `a` and `b`, stored at `link`, name the same
/// path, resolved without touching the filesystem as `link_text` builds them.
pub fn same_link_target(link: &Path, a: &Path, b: &Path) -> bool {
    let dir = link.parent().unwrap_or(link);
    normalize(&dir.join(a)) == normalize(&dir.join(b))
}

/// Text to store in a symlink at `link` that points to `target`. Both paths
/// are absolute and normalized. Relative when they share an ancestor below
/// `/`, absolute otherwise.
pub fn link_text(link: &Path, target: &Path) -> PathBuf {
    let from: Vec<Component> = link.parent().map_or(Vec::new(), |p| p.components().collect());
    let to: Vec<Component> = target.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    // Sharing only the root component means the paths are unrelated trees.
    if common <= 1 {
        return target.to_path_buf();
    }
    let mut text = PathBuf::new();
    for _ in common..from.len() {
        text.push("..");
    }
    for part in &to[common..] {
        text.push(part);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    #[test]
    fn relpath_normalizes() {
        assert_eq!(rel("./a//b/").as_str(), "a/b");
        assert_eq!(rel(".bashrc").as_str(), ".bashrc");
        assert_eq!(rel("a/./b").as_str(), "a/b");
    }

    #[test]
    fn relpath_rejects_parent() {
        assert!(RelPath::parse("a/../b").is_err());
        assert!(RelPath::parse("..").is_err());
    }

    #[test]
    fn relpath_rejects_absolute() {
        let err = RelPath::parse("/etc").unwrap_err();
        assert!(err.to_string().contains("/etc"), "{err}");
    }

    #[test]
    fn relpath_rejects_empty_and_nul() {
        assert!(RelPath::parse("").is_err());
        assert!(RelPath::parse("./").is_err());
        assert!(RelPath::parse("a\0b").is_err());
    }

    #[test]
    fn relpath_navigation() {
        let p = rel(".config/nvim/init.lua");
        assert_eq!(p.file_name(), "init.lua");
        assert_eq!(p.parent(), Some(rel(".config/nvim")));
        assert_eq!(rel(".bashrc").parent(), None);
        assert_eq!(p.ancestors().collect::<Vec<_>>(), [".config/nvim", ".config"]);
        assert_eq!(rel(".bashrc").ancestors().count(), 0);
        assert_eq!(rel(".config").join(&rel("nvim/x")), rel(".config/nvim/x"));
        assert!(p.starts_with(&rel(".config")));
        assert!(p.starts_with(&p));
        assert!(!p.starts_with(&rel(".conf")));
        assert_eq!(p.strip_prefix(&rel(".config")), Some(rel("nvim/init.lua")));
        assert_eq!(p.strip_prefix(&p), None);
        assert_eq!(p.with_file_name("x.lua"), rel(".config/nvim/x.lua"));
        assert_eq!(rel("a").with_file_name("b"), rel("b"));
    }

    #[test]
    fn same_link_target_compares_resolved_paths() {
        let link = Path::new("/h/.config/a");
        let rel = Path::new("../dot/.config/a");
        assert!(same_link_target(link, Path::new("/h/dot/.config/a"), rel));
        assert!(same_link_target(link, Path::new("./../dot/x/../.config/a"), rel));
        assert!(!same_link_target(link, Path::new("/h/dot/.config/b"), rel));
        assert!(!same_link_target(link, Path::new("dot/.config/a"), rel));
    }

    #[test]
    fn link_text_relative_under_home() {
        let t = link_text(Path::new("/h/.bashrc"), Path::new("/h/dot/.bashrc"));
        assert_eq!(t, PathBuf::from("dot/.bashrc"));
    }

    #[test]
    fn link_text_nested() {
        let t = link_text(
            Path::new("/h/.config/nvim/init.lua"),
            Path::new("/h/dot/.config/nvim/init.lua"),
        );
        assert_eq!(t, PathBuf::from("../../dot/.config/nvim/init.lua"));
    }

    #[test]
    fn link_text_absolute_when_only_root_shared() {
        let t = link_text(Path::new("/etc/x"), Path::new("/home/u/dot/x"));
        assert_eq!(t, PathBuf::from("/home/u/dot/x"));
    }

    #[test]
    fn expand_tilde_cases() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/u"));
        assert_eq!(expand_tilde("~/x/y", home), PathBuf::from("/home/u/x/y"));
        assert_eq!(expand_tilde("/abs", home), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("rel", home), PathBuf::from("rel"));
        assert_eq!(expand_tilde("~other/x", home), PathBuf::from("~other/x"));
    }
}
