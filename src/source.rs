//! Source discovery and deployment-mode classification (spec §5).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::config::{Backend, Config, Metadata};
use crate::error::{Error, Result};
use crate::ignore::{Ignore, ROOT_DIR};
use crate::paths::{RelPath, Scope};

/// The one final deployment mode of a source. Encrypted templates are
/// `Template*` with a backend; ordinary links and copies never carry one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Link,
    FileCopy,
    DecryptLink(Backend),
    DecryptCopy(Backend),
    TemplateLink(Option<Backend>),
    TemplateCopy(Option<Backend>, Metadata),
}

impl Mode {
    pub fn backend(&self) -> Option<Backend> {
        match self {
            Mode::Link | Mode::FileCopy => None,
            Mode::DecryptLink(b) | Mode::DecryptCopy(b) => Some(*b),
            Mode::TemplateLink(b) | Mode::TemplateCopy(b, _) => *b,
        }
    }

    /// True when the destination is a regular file rather than a symlink.
    pub fn is_copy(&self) -> bool {
        matches!(self, Mode::FileCopy | Mode::DecryptCopy(_) | Mode::TemplateCopy(..))
    }

    pub fn is_template(&self) -> bool {
        matches!(self, Mode::TemplateLink(_) | Mode::TemplateCopy(..))
    }

    /// Short label used in plans, status and state.
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Link => "link",
            Mode::FileCopy => "copy",
            Mode::DecryptLink(_) => "decrypt+link",
            Mode::DecryptCopy(_) => "decrypt+copy",
            Mode::TemplateLink(_) => "template+link",
            Mode::TemplateCopy(..) => "template+copy",
        }
    }
}

/// Which modes a command deploys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    /// `rustow apply`: every mode.
    All,
    /// `rustow link`: ordinary plaintext symlinks only.
    Link,
    /// `rustow file apply`: registered copies, including decrypt-and-copy.
    File,
    /// `rustow template apply`: templates, including encrypted templates.
    Template,
}

impl Selector {
    /// Same as `includes`, for a `Mode::label()` read back from state.
    pub fn includes_label(self, label: &str) -> bool {
        let known = [
            "link",
            "copy",
            "decrypt+link",
            "decrypt+copy",
            "template+link",
            "template+copy",
        ];
        match self {
            Selector::All => known.contains(&label),
            Selector::Link => label == "link",
            Selector::File => matches!(label, "copy" | "decrypt+copy"),
            Selector::Template => label.starts_with("template+"),
        }
    }

    pub fn includes(self, mode: &Mode) -> bool {
        match self {
            Selector::All => true,
            Selector::Link => *mode == Mode::Link,
            Selector::File => matches!(mode, Mode::FileCopy | Mode::DecryptCopy(_)),
            Selector::Template => mode.is_template(),
        }
    }
}

/// A classified source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Actual repository path (the ciphertext path when encrypted).
    pub path: RelPath,
    pub scope: Scope,
    /// Destination relative to the scope's target root.
    pub dest: RelPath,
    pub mode: Mode,
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// Sorted by path.
    pub sources: Vec<Source>,
    /// User-ignored paths (top-most ignored entry), sorted. Reserved paths are not listed.
    pub ignored: Vec<RelPath>,
}

/// Two or more sources that would write the same destination, or one
/// source whose destination is an ancestor directory of another's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub scope: Scope,
    pub dest: RelPath,
    pub sources: Vec<RelPath>,
}

impl std::fmt::Display for Collision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list: Vec<&str> = self.sources.iter().map(RelPath::as_str).collect();
        write!(
            f,
            "sources {} map to the same destination '{}' ({} scope)",
            list.join(", "),
            self.dest,
            self.scope.name()
        )
    }
}

/// Walks the repository and classifies every non-ignored file. Also checks
/// that each registration and template rule names a deployable source.
pub fn discover(repo: &Path, ignore: &Ignore, cfg: &Config) -> Result<Discovery> {
    let mut out = Discovery::default();
    let mut walker = walkdir::WalkDir::new(repo)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = walker.next() {
        let entry = entry.map_err(|e| Error::Failed(format!("scan repository {}: {e}", repo.display())))?;
        let rel = repo_relative(repo, entry.path())?;
        if visit_entry(rel, entry.file_type(), ignore, cfg, &mut out)? {
            walker.skip_current_dir();
        }
    }
    check_registrations(repo, ignore, cfg, &out.sources)?;
    Ok(out)
}

/// Sorts one walked entry into sources or ignored paths. True when the walker must skip the directory.
fn visit_entry(
    rel: RelPath,
    file_type: std::fs::FileType,
    ignore: &Ignore,
    cfg: &Config,
    out: &mut Discovery,
) -> Result<bool> {
    if ignore.is_ignored(&rel, file_type.is_dir()) {
        if !ignore.is_reserved(&rel, file_type.is_dir()) {
            out.ignored.push(rel);
        }
        return Ok(file_type.is_dir());
    }
    check_kind(&rel, file_type)?;
    if file_type.is_file() {
        out.sources.push(classify(&rel, cfg)?);
    }
    Ok(false)
}

/// Refuses a repository entry that is neither a directory nor a regular file.
pub fn check_kind(rel: &RelPath, file_type: std::fs::FileType) -> Result<()> {
    if file_type.is_file() || file_type.is_dir() {
        return Ok(());
    }
    let what = if file_type.is_symlink() {
        "a symbolic link"
    } else {
        "not a regular file"
    };
    Err(Error::Config(format!(
        "source '{rel}' is {what}; rustow deploys regular files only. Add it to {} to skip it",
        crate::ignore::IGNORE_FILE
    )))
}

fn repo_relative(repo: &Path, path: &Path) -> Result<RelPath> {
    let rel = path.strip_prefix(repo).unwrap_or(path);
    let text = rel.to_str().ok_or_else(|| {
        Error::Config(format!(
            "repository path '{}' is not valid UTF-8; rename it",
            rel.display()
        ))
    })?;
    RelPath::parse(text)
}

/// Every `[[files]]` and `[[template_rules]]` source must be discovered.
fn check_registrations(repo: &Path, ignore: &Ignore, cfg: &Config, sources: &[Source]) -> Result<()> {
    let known: BTreeSet<&RelPath> = sources.iter().map(|s| &s.path).collect();
    let entries = cfg.files.iter().map(|p| ("[[files]]", p));
    let entries = entries.chain(cfg.template_rules.keys().map(|p| ("[[template_rules]]", p)));
    for (table, path) in entries {
        if known.contains(path) {
            continue;
        }
        let why = if ignore.is_reserved(path, false) {
            "is reserved and can never be deployed"
        } else if ignore.is_ignored(path, false) {
            "is ignored by .rustowignore"
        } else if repo.join(path.as_path()).is_dir() {
            "is a directory, not a file"
        } else {
            "does not exist (remove the entry, e.g. with `rustow file remove`)"
        };
        return Err(Error::Config(format!(
            "{}: {table} source '{path}' {why}",
            crate::config::CONFIG_FILE
        )));
    }
    Ok(())
}

/// Classifies one deployable repository file (spec §5 steps 3–8).
pub fn classify(path: &RelPath, cfg: &Config) -> Result<Source> {
    let bad = |why: &str| Error::Config(format!("source '{path}': {why}"));
    let (scope, inner) = split_scope(path)?;
    let (backend, template, name) = split_suffixes(inner.file_name(), &cfg.template_extension).map_err(bad)?;
    let mode = select_mode(path, cfg, backend, template).map_err(|why| bad(&why))?;
    Ok(Source {
        path: path.clone(),
        scope,
        dest: inner.with_file_name(name),
        mode,
    })
}

/// Splits off the `.rustow-root/` prefix: the scope and the path below it.
fn split_scope(path: &RelPath) -> Result<(Scope, RelPath)> {
    match path.as_str().strip_prefix(ROOT_DIR).and_then(|r| r.strip_prefix('/')) {
        Some(rest) => Ok((Scope::Root, RelPath::parse(rest)?)),
        None => Ok((Scope::Home, path.clone())),
    }
}

/// Strips the encryption and template suffixes from a file name: the
/// backend, whether it is a template, and the logical destination name.
fn split_suffixes<'a>(
    file_name: &'a str,
    template_extension: &str,
) -> std::result::Result<(Option<Backend>, bool, &'a str), &'static str> {
    let mut name = file_name;
    let backend = [Backend::Age, Backend::Gpg]
        .into_iter()
        .find(|b| name.ends_with(b.suffix()));
    if let Some(b) = backend {
        name = &name[..name.len() - b.suffix().len()];
        if name.ends_with(Backend::Age.suffix()) || name.ends_with(Backend::Gpg.suffix()) {
            return Err("nested encryption suffixes are not supported");
        }
    }
    let template = name.strip_suffix(template_extension);
    if let Some(stripped) = template {
        name = stripped;
    }
    if name.is_empty() {
        return Err("the destination name is empty after removing suffixes");
    }
    Ok((backend, template.is_some(), name))
}

/// Picks the mode from the suffixes and the configuration; the error is the reason text.
fn select_mode(
    path: &RelPath,
    cfg: &Config,
    backend: Option<Backend>,
    template: bool,
) -> std::result::Result<Mode, String> {
    let registered = cfg.files.contains(path);
    let rule = cfg.template_rules.get(path);
    if template {
        template_mode(backend, registered, rule)
    } else {
        plain_mode(cfg, backend, registered, rule.is_some())
    }
}

fn template_mode(
    backend: Option<Backend>,
    registered: bool,
    rule: Option<&Metadata>,
) -> std::result::Result<Mode, String> {
    if registered {
        return Err(
            "templates cannot be registered with `rustow file add`; use [[template_rules]] metadata to copy them"
                .into(),
        );
    }
    Ok(match rule {
        Some(meta) => Mode::TemplateCopy(backend, meta.clone()),
        None => Mode::TemplateLink(backend),
    })
}

fn plain_mode(
    cfg: &Config,
    backend: Option<Backend>,
    registered: bool,
    has_rule: bool,
) -> std::result::Result<Mode, String> {
    if has_rule {
        return Err(format!(
            "has a [[template_rules]] entry but is not a template (no '{}' suffix)",
            cfg.template_extension
        ));
    }
    Ok(match (backend, registered) {
        (None, false) => Mode::Link,
        (None, true) => Mode::FileCopy,
        (Some(b), false) => Mode::DecryptLink(b),
        (Some(b), true) => Mode::DecryptCopy(b),
    })
}

/// Detects destination collisions across all modes (spec §5 step 9): two
/// sources with one destination, or a destination that is an ancestor
/// directory of another destination.
pub fn check_collisions(sources: &[Source]) -> Vec<Collision> {
    let mut by_dest: BTreeMap<(Scope, &RelPath), Vec<&RelPath>> = BTreeMap::new();
    for s in sources {
        by_dest.entry((s.scope, &s.dest)).or_default().push(&s.path);
    }
    let mut out = Vec::new();
    for ((scope, dest), paths) in &by_dest {
        let mut involved: Vec<RelPath> = paths.iter().map(|p| (*p).clone()).collect();
        for ((other_scope, other_dest), other_paths) in &by_dest {
            if other_scope == scope && other_dest.strip_prefix(dest).is_some() {
                involved.extend(other_paths.iter().map(|p| (*p).clone()));
            }
        }
        if involved.len() > 1 {
            involved.sort();
            out.push(Collision {
                scope: *scope,
                dest: (*dest).clone(),
                sources: involved,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    fn cfg(text: &str) -> Config {
        Config::parse(&format!("version = 1\n{text}")).unwrap()
    }

    fn mode_of(path: &str, c: &Config) -> Mode {
        classify(&rel(path), c).unwrap().mode
    }

    fn repo(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        dir
    }

    fn run(dir: &tempfile::TempDir, c: &Config) -> Result<Discovery> {
        let ig = Ignore::load(dir.path(), vec![]).unwrap();
        discover(dir.path(), &ig, c)
    }

    fn paths(d: &Discovery) -> Vec<&str> {
        d.sources.iter().map(|s| s.path.as_str()).collect()
    }

    #[test]
    fn classify_table() {
        let c = cfg("[[files]]\nsource = \".config/example/settings.json\"\n\
             [[files]]\nsource = \".ssh/copied.conf.age\"\n\
             [[files]]\nsource = \".ssh/g.conf.gpg\"\n\
             [[template_rules]]\nsource = \".ssh/config.tmpl\"\nmode = \"0600\"\n\
             [[template_rules]]\nsource = \".x.json.tmpl.gpg\"\nowner = \"0\"\n");
        let meta600 = c.template_rules[&rel(".ssh/config.tmpl")].clone();
        let meta_owner = c.template_rules[&rel(".x.json.tmpl.gpg")].clone();
        assert_eq!(mode_of(".bashrc", &c), Mode::Link);
        assert_eq!(mode_of(".config/example/settings.json", &c), Mode::FileCopy);
        assert_eq!(mode_of(".gitconfig.tmpl", &c), Mode::TemplateLink(None));
        assert_eq!(mode_of(".ssh/config.tmpl", &c), Mode::TemplateCopy(None, meta600));
        assert_eq!(mode_of(".ssh/private.conf.age", &c), Mode::DecryptLink(Backend::Age));
        assert_eq!(mode_of(".ssh/copied.conf.age", &c), Mode::DecryptCopy(Backend::Age));
        assert_eq!(mode_of(".ssh/g.conf.gpg", &c), Mode::DecryptCopy(Backend::Gpg));
        assert_eq!(mode_of(".s.json.tmpl.age", &c), Mode::TemplateLink(Some(Backend::Age)));
        assert_eq!(
            mode_of(".x.json.tmpl.gpg", &c),
            Mode::TemplateCopy(Some(Backend::Gpg), meta_owner)
        );
        // A plaintext template whose output is named *.age is not encrypted.
        assert_eq!(mode_of("x.age.tmpl", &c), Mode::TemplateLink(None));
    }

    #[test]
    fn classify_destinations() {
        let c = cfg("");
        let s = classify(&rel(".rustowroot/etc/example/config.conf.tmpl"), &c).unwrap();
        assert_eq!((s.scope, s.dest), (Scope::Root, rel("etc/example/config.conf")));
        let s = classify(&rel(".ssh/private.conf.age"), &c).unwrap();
        assert_eq!((s.scope, s.dest), (Scope::Home, rel(".ssh/private.conf")));
        let s = classify(&rel(".config/a.json.tmpl.gpg"), &c).unwrap();
        assert_eq!(s.dest, rel(".config/a.json"));
        assert_eq!(s.path, rel(".config/a.json.tmpl.gpg"));
    }

    #[test]
    fn custom_template_extension() {
        let c = cfg("[templates]\nextension = \".j2\"\n");
        assert_eq!(mode_of("a.j2", &c), Mode::TemplateLink(None));
        assert_eq!(mode_of("a.tmpl", &c), Mode::Link);
    }

    #[test]
    fn nested_encryption_suffix_rejected() {
        let c = cfg("");
        for p in ["a.age.gpg", "a.gpg.age", "a.tmpl.age.age"] {
            let e = classify(&rel(p), &c).unwrap_err().to_string();
            assert!(e.contains(p), "{e}");
        }
    }

    #[test]
    fn empty_logical_name_rejected() {
        let c = cfg("");
        for p in [".age", "d/.gpg", ".tmpl", "d/.tmpl.age"] {
            assert!(classify(&rel(p), &c).is_err(), "{p}");
        }
    }

    #[test]
    fn registered_template_rejected() {
        let c = cfg("[[files]]\nsource = \"a.tmpl\"\n");
        let e = classify(&rel("a.tmpl"), &c).unwrap_err().to_string();
        assert!(e.contains("a.tmpl") && e.contains("template"), "{e}");
        let c = cfg("[[files]]\nsource = \"a.tmpl.age\"\n");
        assert!(classify(&rel("a.tmpl.age"), &c).is_err());
    }

    #[test]
    fn rule_on_non_template_rejected() {
        let c = cfg("[[template_rules]]\nsource = \"a.conf\"\nmode = \"0600\"\n");
        let e = classify(&rel("a.conf"), &c).unwrap_err().to_string();
        assert!(e.contains("a.conf"), "{e}");
    }

    #[test]
    fn selector_filters_recorded_labels() {
        assert!(Selector::Link.includes_label("link"));
        assert!(!Selector::Link.includes_label("decrypt+link"));
        assert!(Selector::File.includes_label("copy"));
        assert!(Selector::File.includes_label("decrypt+copy"));
        assert!(Selector::Template.includes_label("template+link"));
        assert!(Selector::Template.includes_label("template+copy"));
        assert!(!Selector::Template.includes_label("copy"));
        assert!(Selector::All.includes_label("decrypt+link"));
        assert!(!Selector::All.includes_label("bogus"));
    }

    #[test]
    fn selector_filters_modes() {
        let tl = Mode::TemplateLink(Some(Backend::Gpg));
        assert!(Selector::Link.includes(&Mode::Link));
        assert!(!Selector::Link.includes(&Mode::DecryptLink(Backend::Age)));
        assert!(!Selector::Link.includes(&tl));
        assert!(Selector::File.includes(&Mode::FileCopy));
        assert!(Selector::File.includes(&Mode::DecryptCopy(Backend::Age)));
        assert!(!Selector::File.includes(&Mode::DecryptLink(Backend::Age)));
        assert!(Selector::Template.includes(&tl));
        assert!(!Selector::Template.includes(&Mode::FileCopy));
        for m in [Mode::Link, Mode::FileCopy, Mode::DecryptLink(Backend::Gpg), tl] {
            assert!(Selector::All.includes(&m));
        }
    }

    #[test]
    fn discovery_skips_reserved_and_ignored_and_is_sorted() {
        let dir = repo(&[
            ".zshrc",
            ".bashrc",
            ".gitignore",
            ".rustow.toml",
            ".git/HEAD",
            ".rustowcache/rendered/x",
            ".config/nvim/init.lua",
            ".config/nvim/x.swp",
            ".rustowroot/etc/x.conf",
            "private/key",
        ]);
        fs::write(dir.path().join(".rustow.toml"), "version = 1\n").unwrap();
        fs::write(dir.path().join(".rustowignore"), "*.swp\nprivate/\n").unwrap();
        let d = run(&dir, &cfg("")).unwrap();
        assert_eq!(
            paths(&d),
            [".bashrc", ".config/nvim/init.lua", ".rustowroot/etc/x.conf", ".zshrc"]
        );
        let ignored: Vec<&str> = d.ignored.iter().map(RelPath::as_str).collect();
        assert_eq!(ignored, [".config/nvim/x.swp", "private"]);
    }

    #[test]
    fn source_symlink_rejected() {
        let dir = repo(&["real"]);
        std::os::unix::fs::symlink("real", dir.path().join(".vimrc")).unwrap();
        let e = run(&dir, &cfg("")).unwrap_err().to_string();
        assert!(e.contains(".vimrc") && e.contains(".rustowignore"), "{e}");
        fs::write(dir.path().join(".rustowignore"), ".vimrc\n").unwrap();
        assert_eq!(paths(&run(&dir, &cfg("")).unwrap()), ["real"]);
    }

    #[test]
    fn registered_missing_source_rejected() {
        let dir = repo(&[".bashrc"]);
        let e = run(&dir, &cfg("[[files]]\nsource = \"gone.json\"\n"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("gone.json") && e.contains("does not exist"), "{e}");
        let e = run(
            &dir,
            &cfg("[[template_rules]]\nsource = \"gone.tmpl\"\nmode = \"0600\"\n"),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("gone.tmpl"), "{e}");
    }

    #[test]
    fn registered_ignored_or_reserved_source_rejected() {
        let dir = repo(&["a.bak"]);
        fs::write(dir.path().join(".rustowignore"), "*.bak\n").unwrap();
        let e = run(&dir, &cfg("[[files]]\nsource = \"a.bak\"\n"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("a.bak") && e.contains("ignored"), "{e}");
        let e = run(&dir, &cfg("[[files]]\nsource = \".rustowignore\"\n"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("reserved"), "{e}");
    }

    fn src(path: &str, scope: Scope, dest: &str) -> Source {
        Source {
            path: rel(path),
            scope,
            dest: rel(dest),
            mode: Mode::Link,
        }
    }

    #[test]
    fn collision_same_destination() {
        let sources = [
            src(".ssh/x", Scope::Home, ".ssh/x"),
            src(".ssh/x.age", Scope::Home, ".ssh/x"),
            src(".ssh/x.gpg", Scope::Home, ".ssh/x"),
            src(".rustowroot/.ssh/x", Scope::Root, ".ssh/x"),
        ];
        let c = check_collisions(&sources);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].dest, rel(".ssh/x"));
        assert_eq!(c[0].sources, [rel(".ssh/x"), rel(".ssh/x.age"), rel(".ssh/x.gpg")]);
    }

    #[test]
    fn collision_file_vs_dir_destination() {
        let sources = [
            src(".config.tmpl", Scope::Home, ".config"),
            src(".config/x", Scope::Home, ".config/x"),
        ];
        let c = check_collisions(&sources);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].sources, [rel(".config.tmpl"), rel(".config/x")]);
    }
}
