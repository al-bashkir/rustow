//! `.rustow.toml`: typed schema, parsed and validated once at load.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, IoContext, Result};
use crate::paths::RelPath;

pub mod edit;

pub const CONFIG_FILE: &str = ".rustow.toml";
pub const SCHEMA_VERSION: u32 = 1;

/// Encryption backend, chosen by the outer `.age` / `.gpg` source suffix.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Age,
    Gpg,
}

impl Backend {
    pub fn suffix(self) -> &'static str {
        match self {
            Backend::Age => ".age",
            Backend::Gpg => ".gpg",
        }
    }
}

/// A user or group given by name or numeric id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    Name(String),
    Id(u32),
}

/// Explicit file metadata of a template rule. At least one field is set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Metadata {
    pub mode: Option<u32>,
    pub owner: Option<Owner>,
    pub group: Option<Owner>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub target: String,
    pub root_target: String,
    pub cache_dir: String,
    pub state_dir: Option<String>,
    pub create_directories: bool,
    pub fold_directories: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            target: "~".into(),
            root_target: "/".into(),
            cache_dir: ".rustowcache".into(),
            state_dir: None,
            create_directories: true,
            fold_directories: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GpgConfig {
    /// Full public-key fingerprints.
    pub recipients: Vec<String>,
    pub homedir: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Encryption {
    /// Default backend for new encryption.
    pub backend: Backend,
    /// Age public recipients.
    pub recipients: Vec<String>,
    /// External age identity files.
    pub identity_files: Vec<String>,
    pub gpg: GpgConfig,
}

/// Validated configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// True when `.rustow.toml` exists.
    pub present: bool,
    pub defaults: Defaults,
    pub template_extension: String,
    pub encryption: Encryption,
    /// User variables, always a JSON object.
    pub vars: serde_json::Value,
    /// Sources registered for copy deployment.
    pub files: BTreeSet<RelPath>,
    pub template_rules: BTreeMap<RelPath, Metadata>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            present: false,
            defaults: Defaults::default(),
            template_extension: ".tmpl".into(),
            encryption: Encryption::default(),
            vars: serde_json::Value::Object(Default::default()),
            files: BTreeSet::new(),
            template_rules: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Loads `<repo>/.rustow.toml`; an absent file gives the defaults.
    pub fn load(repo: &Path) -> Result<Config> {
        let path = repo.join(CONFIG_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).ctx(|| format!("read {}", path.display())),
        }
    }

    /// Parses the text of an existing `.rustow.toml`.
    pub fn parse(text: &str) -> Result<Config> {
        let raw: Raw = toml_edit::de::from_str(text).map_err(|e| invalid(e.to_string().trim_end()))?;
        check_header(&raw)?;
        let files = parse_files(&raw.files)?;
        let template_rules = parse_template_rules(raw.template_rules)?;
        Ok(Config {
            present: true,
            defaults: raw.defaults,
            template_extension: raw.templates.extension,
            encryption: raw.encryption,
            vars: serde_json::Value::Object(raw.vars),
            files,
            template_rules,
        })
    }
}

/// Checks the schema version, template engine and template extension.
fn check_header(raw: &Raw) -> Result<()> {
    match raw.version {
        Some(SCHEMA_VERSION) => {}
        Some(v) => {
            return Err(invalid(&format!(
                "unsupported schema version {v}; this rustow supports version {SCHEMA_VERSION}"
            )));
        }
        None => return Err(invalid(&format!("missing 'version = {SCHEMA_VERSION}'"))),
    }
    if raw.templates.engine != "minijinja" {
        return Err(invalid(&format!(
            "unsupported template engine '{}'; only \"minijinja\" is available",
            raw.templates.engine
        )));
    }
    validate_extension(&raw.templates.extension)
}

fn parse_files(raw: &[RawFile]) -> Result<BTreeSet<RelPath>> {
    let mut files = BTreeSet::new();
    for (i, f) in raw.iter().enumerate() {
        let path = RelPath::parse(&f.source).map_err(|e| invalid(&format!("files[{i}].source: {e}")))?;
        if !files.insert(path) {
            return Err(invalid(&format!("duplicate [[files]] entry for '{}'", f.source)));
        }
    }
    Ok(files)
}

fn parse_template_rules(raw: Vec<RawRule>) -> Result<BTreeMap<RelPath, Metadata>> {
    let mut template_rules = BTreeMap::new();
    for (i, r) in raw.into_iter().enumerate() {
        let (path, meta) = r
            .validate()
            .map_err(|e| invalid(&format!("template_rules[{i}]: {e}")))?;
        if template_rules.insert(path.clone(), meta).is_some() {
            return Err(invalid(&format!("duplicate [[template_rules]] entry for '{path}'")));
        }
    }
    Ok(template_rules)
}

fn invalid(msg: &str) -> Error {
    Error::Config(format!("{CONFIG_FILE}: {msg}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: Option<u32>,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    templates: RawTemplates,
    #[serde(default)]
    encryption: Encryption,
    #[serde(default)]
    vars: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    template_rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawTemplates {
    extension: String,
    engine: String,
}

impl Default for RawTemplates {
    fn default() -> Self {
        RawTemplates {
            extension: ".tmpl".into(),
            engine: "minijinja".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    source: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    source: String,
    mode: Option<String>,
    owner: Option<String>,
    group: Option<String>,
}

impl RawRule {
    fn validate(self) -> std::result::Result<(RelPath, Metadata), String> {
        let path = RelPath::parse(&self.source).map_err(|e| format!("source: {e}"))?;
        let meta = Metadata {
            mode: self.mode.as_deref().map(parse_mode).transpose()?,
            owner: self.owner.as_deref().map(|o| parse_owner("owner", o)).transpose()?,
            group: self.group.as_deref().map(|g| parse_owner("group", g)).transpose()?,
        };
        if meta.mode.is_none() && meta.owner.is_none() && meta.group.is_none() {
            return Err(format!("'{path}' sets none of mode, owner or group"));
        }
        Ok((path, meta))
    }
}

/// Parses an octal permission string such as "0600". Special bits
/// (setuid, setgid, sticky) are rejected: dotfiles never need them.
fn parse_mode(text: &str) -> std::result::Result<u32, String> {
    let bad = || format!("mode \"{text}\" must be 3 or 4 octal digits no greater than 0777, e.g. \"0600\"");
    // from_str_radix alone would accept a sign.
    if !(3..=4).contains(&text.len()) || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(bad());
    }
    let mode = u32::from_str_radix(text, 8).map_err(|_| bad())?;
    if mode > 0o777 {
        return Err(bad());
    }
    if mode & 0o400 == 0 {
        return Err(format!(
            "mode \"{text}\" lacks owner read (0400); rustow must read the deployed file to detect changes"
        ));
    }
    Ok(mode)
}

fn parse_owner(field: &str, text: &str) -> std::result::Result<Owner, String> {
    if text.is_empty() || text.contains([':', '/']) || text.chars().any(char::is_whitespace) {
        return Err(format!("{field} \"{text}\" is not a valid user or group name"));
    }
    Ok(match text.parse::<u32>() {
        Ok(id) => Owner::Id(id),
        Err(_) => Owner::Name(text.to_owned()),
    })
}

fn validate_extension(ext: &str) -> Result<()> {
    let ok = ext.len() > 1
        && ext.starts_with('.')
        && !ext.contains('/')
        && ![Backend::Age, Backend::Gpg].iter().any(|b| ext.ends_with(b.suffix()));
    if ok {
        Ok(())
    } else {
        Err(invalid(&format!(
            "template extension \"{ext}\" must start with '.', contain no '/', and not end with .age or .gpg"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
version = 1

[defaults]
target = "~"
cache_dir = ".rustowcache"
state_dir = "~/.local/state/rustow"
create_directories = true

[templates]
extension = ".tmpl"
engine = "minijinja"

# Default backend for new encryption: "age" or "gpg".
[encryption]
backend = "age"
recipients = []
identity_files = ["~/.config/rustow/age/identity.txt"]

[encryption.gpg]
recipients = []
homedir = "~/.gnupg"

[vars]
editor = "nvim"
shell = "zsh"

[vars.git]
user_name = "Example User"
user_email = "user@example.com"

[[files]]
source = ".config/example/settings.json"

[[files]]
source = ".config/git/ignore"

[[files]]
source = ".ssh/private.conf.age"

[[template_rules]]
source = ".ssh/config.tmpl"
mode = "0600"

[[template_rules]]
source = ".rustowroot/etc/example/config.conf.tmpl"
mode = "0640"
owner = "root"
group = "root"
"#;

    fn err(text: &str) -> String {
        Config::parse(text).unwrap_err().to_string()
    }

    fn rule_mode(mode: &str) -> Result<Config> {
        Config::parse(&format!(
            "version = 1\n[[template_rules]]\nsource = \"a.tmpl\"\nmode = \"{mode}\"\n"
        ))
    }

    #[test]
    fn empty_config_is_default() {
        let dir = std::env::temp_dir().join("rustow-no-such-dir-for-config-test");
        let cfg = Config::load(&dir).unwrap();
        assert!(!cfg.present);
        assert_eq!(cfg.defaults.cache_dir, ".rustowcache");
        assert_eq!(cfg.template_extension, ".tmpl");
        assert!(cfg.files.is_empty());
    }

    #[test]
    fn example_config_from_spec_parses() {
        let cfg = Config::parse(SPEC_EXAMPLE).unwrap();
        assert!(cfg.present);
        assert_eq!(cfg.vars["git"]["user_name"], "Example User");
        assert_eq!(cfg.files.len(), 3);
        assert!(cfg.files.contains(&RelPath::parse(".ssh/private.conf.age").unwrap()));
        let root = &cfg.template_rules[&RelPath::parse(".rustowroot/etc/example/config.conf.tmpl").unwrap()];
        assert_eq!(root.mode, Some(0o640));
        assert_eq!(root.owner, Some(Owner::Name("root".into())));
        assert_eq!(cfg.encryption.backend, Backend::Age);
        assert_eq!(cfg.encryption.gpg.homedir.as_deref(), Some("~/.gnupg"));
        assert_eq!(cfg.defaults.state_dir.as_deref(), Some("~/.local/state/rustow"));
    }

    #[test]
    fn unknown_key_rejected() {
        let e = err("version = 1\n[defaults]\nfoo = 1\n");
        assert!(e.contains("foo"), "{e}");
        let e = err("version = 1\ncolour = true\n");
        assert!(e.contains("colour"), "{e}");
    }

    #[test]
    fn version_2_rejected() {
        let e = err("version = 2\n");
        assert!(e.contains("version 2"), "{e}");
    }

    #[test]
    fn missing_version_rejected() {
        let e = err("[defaults]\ntarget = \"~\"\n");
        assert!(e.contains("version"), "{e}");
    }

    #[test]
    fn malformed_toml_rejected() {
        let e = err("version = 1\n[defaults\n");
        assert!(e.contains(".rustow.toml"), "{e}");
    }

    #[test]
    fn engine_other_than_minijinja_rejected() {
        let e = err("version = 1\n[templates]\nengine = \"tera\"\n");
        assert!(e.contains("tera"), "{e}");
    }

    #[test]
    fn mode_parses_octal() {
        let p = |m| rule_mode(m).unwrap().template_rules.values().next().unwrap().mode;
        assert_eq!(p("0600"), Some(0o600));
        assert_eq!(p("644"), Some(0o644));
        assert_eq!(p("0755"), Some(0o755));
    }

    #[test]
    fn mode_rejects_bad_values() {
        for bad in ["0999", "04755", "rwx", "", "07777", "+64", "+644", "0200", "044"] {
            let e = rule_mode(bad).unwrap_err().to_string();
            assert!(e.contains("mode"), "{bad}: {e}");
        }
        let e = rule_mode("0200").unwrap_err().to_string();
        assert!(e.contains("owner read") && e.contains("detect changes"), "{e}");
    }

    #[test]
    fn owner_numeric_is_id() {
        let cfg =
            Config::parse("version = 1\n[[template_rules]]\nsource = \"a.tmpl\"\nowner = \"0\"\ngroup = \"wheel\"\n")
                .unwrap();
        let m = cfg.template_rules.values().next().unwrap();
        assert_eq!(m.owner, Some(Owner::Id(0)));
        assert_eq!(m.group, Some(Owner::Name("wheel".into())));
        assert_eq!(m.mode, None);
    }

    #[test]
    fn template_rule_without_metadata_rejected() {
        let e = err("version = 1\n[[template_rules]]\nsource = \"a.tmpl\"\n");
        assert!(e.contains("a.tmpl"), "{e}");
    }

    #[test]
    fn duplicate_entries_rejected() {
        let e = err("version = 1\n[[files]]\nsource = \"a\"\n[[files]]\nsource = \"./a\"\n");
        assert!(e.contains("duplicate"), "{e}");
        let e = err(
            "version = 1\n[[template_rules]]\nsource = \"a.tmpl\"\nmode = \"0600\"\n[[template_rules]]\nsource = \"a.tmpl\"\nmode = \"0644\"\n",
        );
        assert!(e.contains("duplicate"), "{e}");
    }

    #[test]
    fn invalid_source_path_rejected() {
        let e = err("version = 1\n[[files]]\nsource = \"../x\"\n");
        assert!(e.contains("files"), "{e}");
    }

    #[test]
    fn extension_rules() {
        for bad in ["tmpl", ".", ".age", ".gpg", ".tmpl.age", ".a/b"] {
            let e = err(&format!("version = 1\n[templates]\nextension = \"{bad}\"\n"));
            assert!(e.contains("extension"), "{bad}: {e}");
        }
        let cfg = Config::parse("version = 1\n[templates]\nextension = \".j2\"\n").unwrap();
        assert_eq!(cfg.template_extension, ".j2");
    }

    #[test]
    fn backend_defaults_to_age_and_accepts_gpg() {
        assert_eq!(Config::parse("version = 1\n").unwrap().encryption.backend, Backend::Age);
        let cfg = Config::parse("version = 1\n[encryption]\nbackend = \"gpg\"\n").unwrap();
        assert_eq!(cfg.encryption.backend, Backend::Gpg);
        assert!(err("version = 1\n[encryption]\nbackend = \"pgp\"\n").contains("pgp"));
    }

    #[test]
    fn vars_must_be_a_table() {
        let cfg = Config::parse("version = 1\n").unwrap();
        assert!(cfg.vars.is_object());
    }
}
