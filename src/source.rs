//! Source discovery and deployment-mode classification (spec §5).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::config::{Backend, Config, Metadata};
use crate::error::{Error, Result};
use crate::ignore::{Ignore, ROOT_DIR};
use crate::paths::{RelPath, Scope};
use crate::template::{Host, TemplateMeta, split_meta};

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

    /// Short label stored in state and matched by selectors.
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

    /// Label shown in plans, status and lists (see `shown_label`).
    pub fn shown_label(&self) -> String {
        shown_label(self.label(), self.backend())
    }
}

/// A state label with the decrypt step and its backend shown:
/// `decrypt(age)+link`, `decrypt(gpg)+template+copy`.
pub fn shown_label(label: &str, backend: Option<Backend>) -> String {
    match backend {
        Some(b) => {
            let name = b.suffix().trim_start_matches('.');
            format!("decrypt({name})+{}", label.trim_start_matches("decrypt+"))
        }
        None => label.to_owned(),
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

/// A repository file (or a configuration entry) that cannot deploy. The
/// rest of the repository deploys; commands print a warning for it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Skipped {
    pub path: RelPath,
    /// Why it cannot deploy, without the path.
    pub reason: String,
}

const META_ERROR: &str = "template meta block, ";
const META_CONFLICT: &str = "set metadata either in the template meta block or in [[template_rules]], not both";

impl Skipped {
    /// A mistake in a template's meta block (or its clash with a rule):
    /// `check` reports it as a problem, not only as a warning.
    pub fn is_meta_problem(&self) -> bool {
        self.reason.starts_with(META_ERROR) || self.reason == META_CONFLICT
    }
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// Sorted by path.
    pub sources: Vec<Source>,
    /// User-ignored paths (top-most ignored entry), sorted. Reserved paths are not listed.
    pub ignored: Vec<RelPath>,
    /// Files and entries that cannot deploy, sorted by path.
    pub skipped: Vec<Skipped>,
    /// Plaintext templates whose meta condition is false on this host,
    /// sorted by path. Not a problem, so no warning.
    pub inactive: Vec<Skipped>,
    /// `[[file_rules]]` entries that cannot apply, sorted by path. Warnings
    /// only: the path itself may still deploy.
    pub rule_warnings: Vec<Skipped>,
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

/// Walks the repository and classifies every non-ignored file with
/// `classify`. A file that cannot deploy, and a registration or template
/// rule that names no deployable file, go to `skipped`. A plaintext
/// template's meta block is read here; `host` decides its conditions.
pub fn discover(repo: &Path, ignore: &Ignore, cfg: &Config, host: &Host) -> Result<Discovery> {
    let mut out = Discovery::default();
    let mut walker = walkdir::WalkDir::new(repo)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = walker.next() {
        let entry = entry.map_err(|e| Error::Failed(format!("scan repository {}: {e}", repo.display())))?;
        let rel = repo_relative(repo, entry.path())?;
        if visit_entry(rel, entry.file_type(), ignore, cfg, &mut out) {
            walker.skip_current_dir();
        }
    }
    let sources = std::mem::take(&mut out.sources);
    for source in sources {
        place(repo, source, cfg, host, &mut out);
    }
    let unmatched = check_registrations(repo, ignore, cfg, &out);
    out.skipped.extend(unmatched);
    out.skipped.sort();
    out.inactive.sort();
    out.rule_warnings = check_file_rules(repo, ignore, cfg, &out);
    Ok(out)
}

/// Each `[[file_rules]]` entry that cannot control folding, with the reason.
fn check_file_rules(repo: &Path, ignore: &Ignore, cfg: &Config, found: &Discovery) -> Vec<Skipped> {
    cfg.file_rules
        .keys()
        .filter_map(|path| {
            let why = file_rule_problem(repo, ignore, found, path)?;
            Some(Skipped {
                path: path.clone(),
                reason: format!("{} [[file_rules]] entry {why}", crate::config::CONFIG_FILE),
            })
        })
        .collect()
}

/// Why a `[[file_rules]]` entry cannot apply; None for a home link
/// source or a usable directory.
fn file_rule_problem(repo: &Path, ignore: &Ignore, found: &Discovery, path: &RelPath) -> Option<&'static str> {
    const NOT_LINK: &str = "is not a plain symlinked file; fold rules apply only to link sources";
    let p = path.as_str();
    if p == ROOT_DIR || p.strip_prefix(ROOT_DIR).is_some_and(|r| r.starts_with('/')) {
        return Some("is under .rustowroot/, where rustow never folds");
    }
    if let Some(s) = found.sources.iter().find(|s| &s.path == path) {
        return (s.mode != Mode::Link).then_some(NOT_LINK);
    }
    if found.skipped.iter().chain(&found.inactive).any(|s| &s.path == path) {
        return Some(NOT_LINK);
    }
    let is_dir = repo.join(path.as_path()).is_dir();
    if ignore.is_reserved(path, is_dir) {
        Some("is reserved and can never be deployed")
    } else if ignore.ignoring_rule(path, is_dir).is_some() {
        None // not for this host
    } else if ignore.is_ignored(path, is_dir) {
        Some("is ignored by .rustowignore")
    } else if is_dir {
        None
    } else {
        Some("does not exist (remove the entry)")
    }
}

/// Where a classified source goes once its meta block is applied.
#[derive(Debug, PartialEq, Eq)]
enum Placed {
    Deploy(Source),
    /// A condition is false on this host; the reason names it.
    Inactive(String),
    Skip(String),
}

/// Applies a plaintext template's meta block (if any) and files the source.
fn place(repo: &Path, source: Source, cfg: &Config, host: &Host, out: &mut Discovery) {
    let plain_template = matches!(source.mode, Mode::TemplateLink(None) | Mode::TemplateCopy(None, _));
    // Unreadable or non-UTF-8 text: no meta block here; rendering reports the problem.
    let text = plain_template
        .then(|| std::fs::read(repo.join(source.path.as_path())).ok())
        .flatten()
        .and_then(|b| String::from_utf8(b).ok());
    let path = source.path.clone();
    let placed = match text.as_deref().map(split_meta) {
        None => Placed::Deploy(source),
        Some(Err(e)) => Placed::Skip(format!("{META_ERROR}{e}")),
        Some(Ok(block)) => {
            let has_rule = cfg.template_rules.contains_key(&path);
            apply_meta(source, block.meta, has_rule, host)
        }
    };
    match placed {
        Placed::Deploy(s) => out.sources.push(s),
        Placed::Inactive(reason) => out.inactive.push(Skipped { path, reason }),
        Placed::Skip(reason) => out.skipped.push(Skipped { path, reason }),
    }
}

/// The meta block's effect on one plaintext template. Each setting has
/// one home, so meta settings and a `[[template_rules]]` entry conflict.
fn apply_meta(source: Source, meta: TemplateMeta, has_rule: bool, host: &Host) -> Placed {
    if meta.has_settings() && has_rule {
        return Placed::Skip(META_CONFLICT.into());
    }
    if let Some(c) = meta.failed(host) {
        return Placed::Inactive(format!("{c} is false here"));
    }
    if meta.has_settings() {
        let backend = source.mode.backend();
        return Placed::Deploy(Source {
            mode: Mode::TemplateCopy(backend, meta.settings),
            ..source
        });
    }
    Placed::Deploy(source)
}

/// Sorts one walked entry into sources, skipped files or ignored paths. True when the walker must skip the directory.
fn visit_entry(rel: RelPath, file_type: std::fs::FileType, ignore: &Ignore, cfg: &Config, out: &mut Discovery) -> bool {
    if ignore.is_ignored(&rel, file_type.is_dir()) {
        if !ignore.is_reserved(&rel, file_type.is_dir()) {
            out.ignored.push(rel);
        }
        return file_type.is_dir();
    }
    if file_type.is_dir() {
        return false;
    }
    let classified = match unexpected_kind(file_type) {
        Some(what) => Err(format!(
            "is {what}; rustow deploys regular files only. Add it to {} to skip it",
            crate::ignore::IGNORE_FILE
        )),
        None => classify(&rel, cfg),
    };
    match classified {
        Ok(source) => out.sources.push(source),
        Err(reason) => out.skipped.push(Skipped { path: rel, reason }),
    }
    false
}

/// What a repository entry is when it is neither a directory nor a
/// regular file; the caller words the refusal.
pub fn unexpected_kind(file_type: std::fs::FileType) -> Option<&'static str> {
    if file_type.is_file() || file_type.is_dir() {
        None
    } else if file_type.is_symlink() {
        Some("a symbolic link")
    } else {
        Some("not a regular file")
    }
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

/// Each `[[files]]` and `[[template_rules]]` entry that names no
/// discovered or skipped file, with the reason.
fn check_registrations(repo: &Path, ignore: &Ignore, cfg: &Config, found: &Discovery) -> Vec<Skipped> {
    let known: BTreeSet<&RelPath> = found
        .sources
        .iter()
        .map(|s| &s.path)
        .chain(found.skipped.iter().chain(&found.inactive).map(|s| &s.path))
        .collect();
    let entries = cfg.files.iter().map(|p| ("[[files]]", p));
    let entries = entries.chain(cfg.template_rules.keys().map(|p| ("[[template_rules]]", p)));
    entries
        // A path that a matching [[global_rules]] entry ignores is not for this host: no warning.
        .filter(|(_, path)| !known.contains(path) && ignore.ignoring_rule(path, false).is_none())
        .map(|(table, path)| Skipped {
            path: path.clone(),
            reason: format!(
                "{} {table} entry {}",
                crate::config::CONFIG_FILE,
                unmatched_entry(repo, ignore, path)
            ),
        })
        .collect()
}

/// Why a configuration entry names no deployable file.
fn unmatched_entry(repo: &Path, ignore: &Ignore, path: &RelPath) -> &'static str {
    if ignore.is_reserved(path, false) {
        "is reserved and can never be deployed"
    } else if ignore.is_ignored(path, false) {
        "is ignored by .rustowignore"
    } else if repo.join(path.as_path()).is_dir() {
        "is a directory, not a file"
    } else {
        "does not exist (remove the entry, e.g. with `rustow file remove`)"
    }
}

/// Classifies one deployable repository file (spec §5 steps 3–8), with
/// the scope rule applied: the mode it deploys with. The error is the
/// reason the file is skipped.
pub fn classify(path: &RelPath, cfg: &Config) -> std::result::Result<Source, String> {
    let (scope, inner) = split_scope(path).map_err(|e| e.to_string())?;
    let (backend, template, name) = split_suffixes(inner.file_name(), &cfg.template_extension)?;
    let mode = select_mode(path, cfg, backend, template)?;
    Ok(Source {
        path: path.clone(),
        scope,
        dest: inner.with_file_name(name),
        mode: scoped_mode(scope, mode, path)?,
    })
}

/// The one scope rule: a system file never deploys as a symlink. A link
/// into a private cache is unusable for other users, and a link into a
/// user-writable repository is unsafe. A template without a rule becomes
/// a copy with no explicit metadata; any other file must be registered.
fn scoped_mode(scope: Scope, mode: Mode, path: &RelPath) -> std::result::Result<Mode, String> {
    match (scope, mode) {
        (Scope::Root, Mode::TemplateLink(b)) => Ok(Mode::TemplateCopy(b, Metadata::default())),
        (Scope::Root, Mode::Link | Mode::DecryptLink(_)) => Err(format!(
            "system files deploy only as copies; register it with `rustow file add {path}`"
        )),
        (_, mode) => Ok(mode),
    }
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
pub fn split_suffixes<'a>(
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
/// directory of another destination. `folded(scope)` says that the
/// scope's target ignores case; its destinations are compared case-folded.
pub fn check_collisions(sources: &[Source], folded: impl Fn(Scope) -> bool) -> Vec<Collision> {
    let mut by_dest: BTreeMap<(Scope, String), Vec<&Source>> = BTreeMap::new();
    for s in sources {
        let key = collision_key(s.dest.as_str(), folded(s.scope));
        by_dest.entry((s.scope, key)).or_default().push(s);
    }
    let mut out = Vec::new();
    for ((scope, dest), group) in &by_dest {
        let mut involved: Vec<RelPath> = group.iter().map(|s| s.path.clone()).collect();
        for ((other_scope, other_dest), others) in &by_dest {
            let below = other_dest
                .strip_prefix(dest.as_str())
                .is_some_and(|r| r.starts_with('/'));
            if other_scope == scope && below {
                involved.extend(others.iter().map(|s| s.path.clone()));
            }
        }
        if involved.len() > 1 {
            involved.sort();
            out.push(Collision {
                scope: *scope,
                dest: group[0].dest.clone(),
                sources: involved,
            });
        }
    }
    out
}

/// A destination path as compared for collisions: case-folded when the
/// target ignores case.
pub fn collision_key(path: &str, folded: bool) -> String {
    if folded { path.to_lowercase() } else { path.to_owned() }
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
        discover(dir.path(), &ig, c, &test_host())
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
    fn root_sources_never_link() {
        let c = cfg("[[files]]\nsource = \".rustowroot/etc/c\"\n\
             [[template_rules]]\nsource = \".rustowroot/etc/r.tmpl\"\nmode = \"0600\"\n");
        let rule = c.template_rules[&rel(".rustowroot/etc/r.tmpl")].clone();
        let none = Metadata::default();
        assert_eq!(
            mode_of(".rustowroot/etc/t.tmpl", &c),
            Mode::TemplateCopy(None, none.clone())
        );
        assert_eq!(
            mode_of(".rustowroot/etc/t.tmpl.age", &c),
            Mode::TemplateCopy(Some(Backend::Age), none)
        );
        assert_eq!(mode_of(".rustowroot/etc/r.tmpl", &c), Mode::TemplateCopy(None, rule));
        assert_eq!(mode_of(".rustowroot/etc/c", &c), Mode::FileCopy);
        for p in [".rustowroot/etc/l", ".rustowroot/etc/l.gpg"] {
            assert_eq!(
                classify(&rel(p), &c).unwrap_err(),
                format!("system files deploy only as copies; register it with `rustow file add {p}`")
            );
        }
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
            let e = classify(&rel(p), &c).unwrap_err();
            assert_eq!(e, "nested encryption suffixes are not supported", "{p}");
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
        let e = classify(&rel("a.tmpl"), &c).unwrap_err();
        assert!(e.starts_with("templates cannot be registered"), "{e}");
        let c = cfg("[[files]]\nsource = \"a.tmpl.age\"\n");
        assert!(classify(&rel("a.tmpl.age"), &c).is_err());
    }

    #[test]
    fn rule_on_non_template_rejected() {
        let c = cfg("[[template_rules]]\nsource = \"a.conf\"\nmode = \"0600\"\n");
        let e = classify(&rel("a.conf"), &c).unwrap_err();
        assert_eq!(
            e,
            "has a [[template_rules]] entry but is not a template (no '.tmpl' suffix)"
        );
    }

    #[test]
    fn shown_labels_name_the_decrypt_step_and_backend() {
        let meta =
            cfg("[[template_rules]]\nsource = \"a.tmpl\"\nmode = \"0600\"\n").template_rules[&rel("a.tmpl")].clone();
        let cases = [
            (Mode::Link, "link"),
            (Mode::FileCopy, "copy"),
            (Mode::DecryptLink(Backend::Age), "decrypt(age)+link"),
            (Mode::DecryptCopy(Backend::Gpg), "decrypt(gpg)+copy"),
            (Mode::TemplateLink(None), "template+link"),
            (Mode::TemplateLink(Some(Backend::Gpg)), "decrypt(gpg)+template+link"),
            (
                Mode::TemplateCopy(Some(Backend::Age), meta),
                "decrypt(age)+template+copy",
            ),
        ];
        for (mode, shown) in cases {
            assert_eq!(mode.shown_label(), shown);
            assert_eq!(shown_label(mode.label(), mode.backend()), shown, "from a state record");
        }
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
        let d = run(&dir, &cfg("[[files]]\nsource = \".rustowroot/etc/x.conf\"\n")).unwrap();
        assert_eq!(
            paths(&d),
            [".bashrc", ".config/nvim/init.lua", ".rustowroot/etc/x.conf", ".zshrc"]
        );
        let ignored: Vec<&str> = d.ignored.iter().map(RelPath::as_str).collect();
        assert_eq!(ignored, [".config/nvim/x.swp", "private"]);
    }

    fn skipped(d: &Discovery) -> Vec<String> {
        d.skipped.iter().map(|s| format!("{}: {}", s.path, s.reason)).collect()
    }

    /// A file that cannot deploy is skipped with its reason; discovery
    /// still finds every other source.
    #[test]
    fn discovery_skips_each_file_that_cannot_deploy() {
        let dir = repo(&[
            ".bashrc",
            "real",
            ".rustowroot/etc/x",
            "n.age.gpg",
            "a.conf",
            "t.tmpl",
            ".age",
        ]);
        std::os::unix::fs::symlink("real", dir.path().join(".vimrc")).unwrap();
        let c = cfg("[[files]]\nsource = \"t.tmpl\"\n[[template_rules]]\nsource = \"a.conf\"\nmode = \"0600\"\n");
        let d = run(&dir, &c).unwrap();
        assert_eq!(paths(&d), [".bashrc", "real"]);
        assert_eq!(
            skipped(&d),
            [
                ".age: the destination name is empty after removing suffixes",
                ".rustowroot/etc/x: system files deploy only as copies; register it with `rustow file add .rustowroot/etc/x`",
                ".vimrc: is a symbolic link; rustow deploys regular files only. Add it to .rustowignore to skip it",
                "a.conf: has a [[template_rules]] entry but is not a template (no '.tmpl' suffix)",
                "n.age.gpg: nested encryption suffixes are not supported",
                "t.tmpl: templates cannot be registered with `rustow file add`; use [[template_rules]] metadata to copy them",
            ]
        );
        fs::write(dir.path().join(".rustowignore"), ".vimrc\n").unwrap();
        let d = run(&dir, &c).unwrap();
        assert!(!skipped(&d).iter().any(|s| s.starts_with(".vimrc")), "{d:?}");
    }

    /// A `[[files]]` or `[[template_rules]]` entry that names no
    /// deployable file is skipped; a skipped file is named only once.
    #[test]
    fn registrations_without_a_deployable_file_are_skipped() {
        let dir = repo(&[".bashrc", "a.bak", "d/f", "n.age.gpg"]);
        fs::write(dir.path().join(".rustowignore"), "*.bak\n").unwrap();
        let c = cfg("[[files]]\nsource = \"gone.json\"\n[[files]]\nsource = \"a.bak\"\n\
             [[files]]\nsource = \".rustowignore\"\n[[files]]\nsource = \"d\"\n\
             [[files]]\nsource = \"n.age.gpg\"\n\
             [[template_rules]]\nsource = \"gone.tmpl\"\nmode = \"0600\"\n");
        let d = run(&dir, &c).unwrap();
        assert_eq!(paths(&d), [".bashrc", "d/f"]);
        let gone = "does not exist (remove the entry, e.g. with `rustow file remove`)";
        assert_eq!(
            skipped(&d),
            [
                ".rustowignore: .rustow.toml [[files]] entry is reserved and can never be deployed".to_owned(),
                "a.bak: .rustow.toml [[files]] entry is ignored by .rustowignore".to_owned(),
                "d: .rustow.toml [[files]] entry is a directory, not a file".to_owned(),
                format!("gone.json: .rustow.toml [[files]] entry {gone}"),
                format!("gone.tmpl: .rustow.toml [[template_rules]] entry {gone}"),
                "n.age.gpg: nested encryption suffixes are not supported".to_owned(),
            ]
        );
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
        let c = check_collisions(&sources, |_| false);
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
        let c = check_collisions(&sources, |_| false);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].sources, [rel(".config.tmpl"), rel(".config/x")]);
    }

    #[test]
    fn collision_keys_ignore_case_only_on_a_case_insensitive_target() {
        let sources = [
            src("X", Scope::Home, "X"),
            src("x.tmpl", Scope::Home, "x"),
            src(".Config.tmpl", Scope::Home, ".Config"),
            src(".config/a", Scope::Home, ".config/a"),
            src(".rustowroot/x", Scope::Root, "x"),
        ];
        assert!(check_collisions(&sources, |_| false).is_empty());
        let c = check_collisions(&sources, |scope| scope == Scope::Home);
        let found: Vec<&[RelPath]> = c.iter().map(|c| c.sources.as_slice()).collect();
        assert_eq!(
            found,
            [
                &[rel(".Config.tmpl"), rel(".config/a")][..],
                &[rel("X"), rel("x.tmpl")][..]
            ],
            "the root scope is case-sensitive here"
        );
    }

    fn test_host() -> Host {
        Host {
            os: "linux".into(),
            arch: "x86_64".into(),
            family: "unix".into(),
            hostname: "laptop".into(),
            username: "me".into(),
        }
    }

    fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (f, body) in files {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        dir
    }

    #[test]
    fn meta_settings_make_a_template_copy() {
        let dir = repo_with(&[(".x.conf.tmpl", "{%- meta.mode = \"0600\" %}\nx\n")]);
        let d = run(&dir, &cfg("")).unwrap();
        let want = Metadata {
            mode: Some(0o600),
            ..Default::default()
        };
        assert_eq!(d.sources[0].mode, Mode::TemplateCopy(None, want));
    }

    #[test]
    fn meta_settings_replace_the_default_metadata_of_a_system_template() {
        let dir = repo_with(&[(".rustowroot/etc/x.tmpl", "{%- meta.owner = \"root\" %}\nx\n")]);
        let d = run(&dir, &cfg("")).unwrap();
        let want = Metadata {
            owner: Some(crate::config::Owner::Name("root".into())),
            ..Default::default()
        };
        assert_eq!(d.sources[0].mode, Mode::TemplateCopy(None, want));
    }

    #[test]
    fn conditions_only_keep_the_normal_mode() {
        let dir = repo_with(&[(".x.tmpl", "{%- meta.os == \"linux\" %}\nx\n")]);
        assert_eq!(run(&dir, &cfg("")).unwrap().sources[0].mode, Mode::TemplateLink(None));
    }

    #[test]
    fn false_condition_goes_to_inactive_not_skipped() {
        let dir = repo_with(&[(".x.tmpl", "{%- meta.os == \"plan9\" %}\nx\n")]);
        let d = run(&dir, &cfg("")).unwrap();
        assert!(d.sources.is_empty() && d.skipped.is_empty(), "{d:?}");
        assert_eq!(d.inactive[0].path.as_str(), ".x.tmpl");
        assert_eq!(d.inactive[0].reason, "meta.os == \"plan9\" is false here");
    }

    #[test]
    fn settings_plus_rule_is_skipped_but_conditions_plus_rule_is_fine() {
        let c = cfg("[[template_rules]]\nsource = \"a.tmpl\"\nmode = \"0600\"\n\
                     [[template_rules]]\nsource = \"b.tmpl\"\nmode = \"0600\"\n");
        let dir = repo_with(&[
            ("a.tmpl", "{%- meta.uid = 1 %}\n"),
            ("b.tmpl", "{%- meta.os == \"linux\" %}\n"),
        ]);
        let d = run(&dir, &c).unwrap();
        assert!(
            d.skipped
                .iter()
                .any(|s| s.path.as_str() == "a.tmpl" && s.reason.contains("not both")),
            "{d:?}"
        );
        assert_eq!(paths(&d), vec!["b.tmpl"]);
    }

    #[test]
    fn rule_on_an_inactive_template_is_not_reported_missing() {
        let c = cfg("[[template_rules]]\nsource = \"a.tmpl\"\nmode = \"0600\"\n");
        let dir = repo_with(&[("a.tmpl", "{%- meta.os == \"plan9\" %}\n")]);
        assert!(run(&dir, &c).unwrap().skipped.is_empty());
    }

    #[test]
    fn bad_meta_block_skips_with_line() {
        let dir = repo_with(&[("a.tmpl", "{%- meta.color = \"red\" %}\n")]);
        let d = run(&dir, &cfg("")).unwrap();
        assert!(
            d.skipped[0].reason.starts_with("template meta block, line 1: "),
            "{}",
            d.skipped[0].reason
        );
    }

    #[test]
    fn encrypted_templates_are_not_read_for_meta() {
        let dir = repo_with(&[("a.tmpl.age", "{%- meta.os == \"plan9\" %}\n")]);
        assert_eq!(paths(&run(&dir, &cfg("")).unwrap()), vec!["a.tmpl.age"]);
    }

    #[test]
    fn plain_files_are_not_read_for_meta() {
        let dir = repo_with(&[("a.conf", "{%- meta.os == \"plan9\" %}\n")]);
        assert_eq!(paths(&run(&dir, &cfg("")).unwrap()), vec!["a.conf"]);
    }

    fn rule_reasons(files: &[(&str, &str)], config: &str) -> Vec<(String, String)> {
        let dir = repo_with(files);
        let d = run(&dir, &cfg(config)).unwrap();
        assert!(
            d.skipped.is_empty(),
            "rule problems never become skipped records: {d:?}"
        );
        d.rule_warnings
            .iter()
            .map(|w| (w.path.as_str().to_owned(), w.reason.clone()))
            .collect()
    }

    fn rule(source: &str) -> String {
        format!("[[file_rules]]\nsource = \"{source}\"\nfold_directories = false\n")
    }

    #[test]
    fn file_rules_on_link_sources_and_directories_are_fine() {
        let files = [(".config/a/b.txt", "x"), (".config/a/c.tmpl", "x")];
        let config = rule(".config/a/b.txt") + &rule(".config/a") + &rule(".config");
        assert!(rule_reasons(&files, &config).is_empty());
    }

    #[test]
    fn file_rule_problems_are_warnings() {
        let files = [
            (".rustowignore", "*.bak\n"),
            ("t.tmpl", "x"),
            ("off.tmpl", "{%- meta.os == \"plan9\" %}\n"),
            ("c.conf", "x"),
            ("x.bak", "x"),
            (".rustowroot/etc/x.tmpl", "x"),
        ];
        let config = String::from("[[files]]\nsource = \"c.conf\"\n")
            + &rule("t.tmpl")
            + &rule("off.tmpl")
            + &rule("c.conf")
            + &rule("x.bak")
            + &rule(".rustowroot/etc")
            + &rule(".rustowroot")
            + &rule("gone")
            + &rule(".git/x");
        let entry = ".rustow.toml [[file_rules]] entry";
        let not_link = format!("{entry} is not a plain symlinked file; fold rules apply only to link sources");
        let root = format!("{entry} is under .rustowroot/, where rustow never folds");
        assert_eq!(
            rule_reasons(&files, &config),
            vec![
                (
                    ".git/x".into(),
                    format!("{entry} is reserved and can never be deployed")
                ),
                (".rustowroot".into(), root.clone()),
                (".rustowroot/etc".into(), root),
                ("c.conf".into(), not_link.clone()),
                ("gone".into(), format!("{entry} does not exist (remove the entry)")),
                ("off.tmpl".into(), not_link.clone()),
                ("t.tmpl".into(), not_link),
                ("x.bak".into(), format!("{entry} is ignored by .rustowignore")),
            ]
        );
    }
}
