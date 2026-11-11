//! Conservative text edits of `.rustow.toml` and ignore files. Edits keep
//! comments, formatting and line endings; `None` means "already right".

use ::ignore::gitignore::GitignoreBuilder;
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use super::{CONFIG_FILE, Config, SCHEMA_VERSION};
use crate::error::{Error, Result};
use crate::paths::RelPath;

/// Text of a new minimal configuration.
pub fn minimal() -> String {
    format!(
        "# rustow configuration. See docs/configuration.md.\n\
         version = {SCHEMA_VERSION}\n\n\
         [defaults]\n\
         # Generated files (rendered templates, decrypted secrets). Never deployed.\n\
         cache_dir = \".rustowcache\"\n"
    )
}

/// Adds `version = 1` to an existing configuration that lacks it, then
/// validates the result. Malformed or unsupported configuration is an error.
pub fn ensure_init(text: &str) -> Result<Option<String>> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|e| Error::Config(format!("{CONFIG_FILE}: {e}")))?;
    let updated = if doc.contains_key("version") {
        None
    } else {
        let nl = newline(text);
        let gap = if text.is_empty() { "" } else { nl };
        Some(format!("version = {SCHEMA_VERSION}{nl}{gap}{text}"))
    };
    Config::parse(updated.as_deref().unwrap_or(text))?;
    Ok(updated)
}

/// Appends `/<dir>/` to gitignore-style `text` unless its own rules
/// already exclude the whole `dir` subtree (built-in rules do not count).
pub fn ensure_ignore_entry(text: &str, dir: &RelPath) -> Option<String> {
    let mut builder = GitignoreBuilder::new("/");
    for line in text.lines() {
        // Git skips invalid patterns; so do we.
        let _ = builder.add_line(None, line);
    }
    if builder
        .build()
        .is_ok_and(|m| crate::ignore::excludes(&m, dir, true))
    {
        return None;
    }
    let nl = newline(text);
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    out.push_str(&format!("/{dir}/{nl}"));
    Some(out)
}

/// Adds a `[[files]]` entry for `source`. `None` when already registered.
/// `text` is the current configuration ("" when the file is absent).
pub fn add_file(text: &str, source: &RelPath) -> Result<Option<String>> {
    let base = if text.trim().is_empty() {
        minimal()
    } else {
        text.to_owned()
    };
    let config = Config::parse(&base)?;
    if config.files.contains(source) {
        return Ok(None);
    }
    let mut doc = parse_doc(&base)?;
    match doc.get_mut("files") {
        None => {
            let mut tables = ArrayOfTables::new();
            tables.push(file_table(source));
            doc.insert("files", Item::ArrayOfTables(tables));
        }
        Some(Item::ArrayOfTables(tables)) => tables.push(file_table(source)),
        Some(Item::Value(Value::Array(array))) => {
            let mut entry = InlineTable::new();
            entry.insert("source", source.as_str().into());
            array.push(entry);
        }
        Some(_) => {
            return Err(Error::Config(format!(
                "{CONFIG_FILE}: 'files' must be an array of tables"
            )));
        }
    }
    let out = doc.to_string();
    Config::parse(&out)?;
    Ok(Some(out))
}

/// Removes every `[[files]]` entry for `source`. `None` when not registered.
pub fn remove_file(text: &str, source: &RelPath) -> Result<Option<String>> {
    if !Config::parse(text)?.files.contains(source) {
        return Ok(None);
    }
    let names = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .and_then(|s| RelPath::parse(s).ok())
    };
    let mut doc = parse_doc(text)?;
    match doc.get_mut("files") {
        Some(Item::ArrayOfTables(tables)) => {
            tables.retain(|t| {
                names(t.get("source").and_then(Item::as_value)).as_ref() != Some(source)
            });
            if tables.is_empty() {
                doc.remove("files");
            }
        }
        Some(Item::Value(Value::Array(array))) => {
            array.retain(|v| {
                names(v.as_inline_table().and_then(|t| t.get("source"))).as_ref() != Some(source)
            });
        }
        _ => {}
    }
    let out = doc.to_string();
    Config::parse(&out)?;
    Ok(Some(out))
}

fn parse_doc(text: &str) -> Result<DocumentMut> {
    text.parse()
        .map_err(|e| Error::Config(format!("{CONFIG_FILE}: {e}")))
}

fn file_table(source: &RelPath) -> Table {
    let mut table = Table::new();
    table.insert("source", toml_edit::value(source.as_str()));
    table.decor_mut().set_prefix("\n");
    table
}

fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> RelPath {
        RelPath::parse(".rustowcache").unwrap()
    }

    #[test]
    fn minimal_config_is_valid() {
        let cfg = Config::parse(&minimal()).unwrap();
        assert_eq!(cfg.defaults.cache_dir, ".rustowcache");
        assert!(cfg.files.is_empty() && cfg.vars.as_object().unwrap().is_empty());
    }

    #[test]
    fn ensure_init_adds_only_version_and_keeps_comments() {
        let text = "# mine\n[defaults]\ntarget = \"~\" # keep me\n";
        let out = ensure_init(text).unwrap().unwrap();
        assert!(out.starts_with("version = 1\n"), "{out}");
        assert!(out.ends_with(text), "{out}");
        assert_eq!(ensure_init(&out).unwrap(), None);
    }

    #[test]
    fn ensure_init_keeps_crlf() {
        let out = ensure_init("[vars]\r\na = 1\r\n").unwrap().unwrap();
        assert_eq!(out, "version = 1\r\n\r\n[vars]\r\na = 1\r\n");
    }

    #[test]
    fn ensure_init_leaves_complete_config_alone() {
        assert_eq!(ensure_init("version = 1\n").unwrap(), None);
        assert_eq!(ensure_init(&minimal()).unwrap(), None);
    }

    #[test]
    fn ensure_init_rejects_bad_config() {
        assert!(ensure_init("version = 1\n[defaults\n").is_err());
        assert!(ensure_init("version = 2\n").is_err());
        assert!(ensure_init("bogus = true\n").is_err());
    }

    #[test]
    fn ignore_entry_appended_to_empty_and_unrelated() {
        assert_eq!(
            ensure_ignore_entry("", &cache()).unwrap(),
            "/.rustowcache/\n"
        );
        assert_eq!(
            ensure_ignore_entry("target/\n", &cache()).unwrap(),
            "target/\n/.rustowcache/\n"
        );
    }

    #[test]
    fn ignore_entry_handles_missing_final_newline() {
        assert_eq!(
            ensure_ignore_entry("node_modules", &cache()).unwrap(),
            "node_modules\n/.rustowcache/\n"
        );
    }

    #[test]
    fn ignore_entry_keeps_crlf() {
        assert_eq!(
            ensure_ignore_entry("a\r\nb\r\n", &cache()).unwrap(),
            "a\r\nb\r\n/.rustowcache/\r\n"
        );
    }

    #[test]
    fn equivalent_entries_are_recognized() {
        for text in [
            "/.rustowcache/\n",
            ".rustowcache\n",
            "/.rustowcache\n",
            ".rustowcache*\n",
            ".rustow*/\n",
            "*\n",
        ] {
            assert_eq!(ensure_ignore_entry(text, &cache()), None, "{text:?}");
        }
    }

    #[test]
    fn negation_is_repaired_once() {
        let out = ensure_ignore_entry(".rustowcache/\n!.rustowcache/\n", &cache()).unwrap();
        assert_eq!(out, ".rustowcache/\n!.rustowcache/\n/.rustowcache/\n");
        assert_eq!(ensure_ignore_entry(&out, &cache()), None);
    }

    #[test]
    fn child_negation_does_not_count_as_reinclusion() {
        // Git cannot re-include a child of an excluded directory.
        assert_eq!(
            ensure_ignore_entry("/.rustowcache/\n!/.rustowcache/keep\n", &cache()),
            None
        );
    }

    fn rel(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    #[test]
    fn add_file_appends_entry_and_keeps_comments() {
        let text = "# top\nversion = 1\n\n[vars]\na = 1 # note\n";
        let out = add_file(text, &rel(".config/x.json")).unwrap().unwrap();
        assert!(out.starts_with(text), "{out}");
        assert!(
            out.contains("[[files]]\nsource = \".config/x.json\"\n"),
            "{out}"
        );
        let cfg = Config::parse(&out).unwrap();
        assert!(cfg.files.contains(&rel(".config/x.json")));
    }

    #[test]
    fn add_file_is_idempotent_with_equivalent_spelling() {
        let text = "version = 1\n[[files]]\nsource = \"./a//b\"\n";
        assert_eq!(add_file(text, &rel("a/b")).unwrap(), None);
    }

    #[test]
    fn add_file_to_absent_config_creates_minimal() {
        let out = add_file("", &rel("x")).unwrap().unwrap();
        let cfg = Config::parse(&out).unwrap();
        assert!(cfg.files.contains(&rel("x")));
    }

    #[test]
    fn add_file_supports_inline_array() {
        let out = add_file("version = 1\nfiles = [{ source = \"a\" }]\n", &rel("b"))
            .unwrap()
            .unwrap();
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(cfg.files.len(), 2, "{out}");
    }

    #[test]
    fn remove_file_drops_only_that_entry() {
        let text = "# keep\nversion = 1\n[[files]]\nsource = \"a\"\n\n[[files]]\nsource = \"b\"\n";
        let out = remove_file(text, &rel("a")).unwrap().unwrap();
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(
            cfg.files.iter().map(RelPath::as_str).collect::<Vec<_>>(),
            ["b"]
        );
        assert!(out.contains("# keep"), "{out}");
        assert_eq!(remove_file(&out, &rel("a")).unwrap(), None);
        let out = remove_file(&out, &rel("b")).unwrap().unwrap();
        assert!(Config::parse(&out).unwrap().files.is_empty(), "{out}");
    }

    #[test]
    fn edits_reject_invalid_config() {
        assert!(add_file("version = 1\n[defaults\n", &rel("a")).is_err());
        assert!(remove_file("version = 9\n", &rel("a")).is_err());
        assert!(add_file("version = 1\nfiles = 3\n", &rel("a")).is_err());
    }

    #[test]
    fn nested_custom_cache_dir() {
        let dir = RelPath::parse("var/cache").unwrap();
        assert_eq!(ensure_ignore_entry("var/\n", &dir), None);
        assert_eq!(ensure_ignore_entry("", &dir).unwrap(), "/var/cache/\n");
    }
}
