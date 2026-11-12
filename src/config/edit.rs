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
    let doc = parse_doc(text)?;
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
    if builder.build().is_ok_and(|m| crate::ignore::excludes(&m, dir, true)) {
        return None;
    }
    let nl = newline(text);
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(nl);
    }
    out.push_str(&format!("{}{nl}", ignore_entry(dir)));
    Some(out)
}

/// The anchored gitignore entry `/<dir>/`, glob characters escaped. It
/// ends with '/', so no space in it is trailing.
pub fn ignore_entry(dir: &RelPath) -> String {
    let mut entry = String::from("/");
    for c in dir.as_str().chars() {
        if matches!(c, '\\' | '*' | '?' | '[') {
            entry.push('\\');
        }
        entry.push(c);
    }
    entry.push('/');
    entry
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
    append_entry(&mut doc, source)?;
    finish(&doc, &base)
}

/// Appends a `[[files]]` entry to the document, in the form its `files` key already has.
fn append_entry(doc: &mut DocumentMut, source: &RelPath) -> Result<()> {
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
    Ok(())
}

/// Removes every `[[files]]` entry for `source`. `None` when not registered.
pub fn remove_file(text: &str, source: &RelPath) -> Result<Option<String>> {
    if !Config::parse(text)?.files.contains(source) {
        return Ok(None);
    }
    let names = |v: Option<&Value>| v.and_then(Value::as_str).and_then(|s| RelPath::parse(s).ok());
    let mut doc = parse_doc(text)?;
    match doc.get_mut("files") {
        Some(Item::ArrayOfTables(tables)) => {
            let removed = |t: &Table| names(t.get("source").and_then(Item::as_value)).as_ref() == Some(source);
            // Text above a removed header (comments) moves to the next kept entry.
            let mut carried = String::new();
            for table in tables.iter_mut() {
                let prefix = table
                    .decor()
                    .prefix()
                    .and_then(|p| p.as_str())
                    .unwrap_or_default()
                    .to_owned();
                if removed(table) {
                    carried.push_str(&prefix);
                } else if !carried.is_empty() {
                    table.decor_mut().set_prefix(std::mem::take(&mut carried) + &prefix);
                }
            }
            tables.retain(|t| !removed(t));
            if tables.is_empty() {
                doc.remove("files");
            }
            if !carried.trim().is_empty() {
                let trailing = doc.trailing().as_str().unwrap_or_default();
                doc.set_trailing(format!("{trailing}{carried}"));
            }
        }
        Some(Item::Value(Value::Array(array))) => {
            array.retain(|v| names(v.as_inline_table().and_then(|t| t.get("source"))).as_ref() != Some(source));
        }
        _ => {}
    }
    finish(&doc, text)
}

/// Points every `[[files]]` and `[[template_rules]]` entry for `from` at
/// `to`, keeping the other keys of the entry. `None` when nothing refers to `from`.
pub fn rename_source(text: &str, from: &RelPath, to: &RelPath) -> Result<Option<String>> {
    Config::parse(text)?;
    let mut doc = parse_doc(text)?;
    let renamed: usize = ["files", "template_rules"]
        .into_iter()
        .map(|key| rename_in_key(doc.get_mut(key), from, to))
        .sum();
    if renamed == 0 {
        return Ok(None);
    }
    finish(&doc, text)
}

/// The edited text with the line endings of `original` (toml_edit writes
/// LF), validated.
fn finish(doc: &DocumentMut, original: &str) -> Result<Option<String>> {
    let mut out = doc.to_string();
    if newline(original) == "\r\n" {
        out = out.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    Config::parse(&out)?;
    Ok(Some(out))
}

/// True when the TOML value is a path equal to `source`.
fn names_source(value: Option<&Value>, source: &RelPath) -> bool {
    value
        .and_then(Value::as_str)
        .and_then(|s| RelPath::parse(s).ok())
        .as_ref()
        == Some(source)
}

/// Renames the matching entries of one array key; returns how many changed.
/// The value is replaced in place: `insert` would drop the comments around it.
fn rename_in_key(item: Option<&mut Item>, from: &RelPath, to: &RelPath) -> usize {
    let sources: Vec<&mut Value> = match item {
        Some(Item::ArrayOfTables(tables)) => tables
            .iter_mut()
            .filter_map(|t| t.get_mut("source")?.as_value_mut())
            .collect(),
        Some(Item::Value(Value::Array(array))) => array
            .iter_mut()
            .filter_map(|v| v.as_inline_table_mut()?.get_mut("source"))
            .collect(),
        _ => Vec::new(),
    };
    let mut renamed = 0;
    for value in sources.into_iter().filter(|v| names_source(Some(v), from)) {
        let decor = value.decor().clone();
        *value = to.as_str().into();
        *value.decor_mut() = decor;
        renamed += 1;
    }
    renamed
}

fn parse_doc(text: &str) -> Result<DocumentMut> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| super::toml_error(text, e.message(), e.span()))
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
    fn syntax_errors_name_the_place_but_no_value() {
        let text = "version = 1\n[vars]\nt = \"S3CRET\" x\n";
        for e in [ensure_init(text).unwrap_err(), parse_doc(text).unwrap_err()] {
            let e = e.to_string();
            assert!(e.contains("line 3, column 14, key vars.t"), "{e}");
            assert!(!e.contains("S3CRET"), "{e}");
        }
    }

    #[test]
    fn ensure_init_rejects_bad_config() {
        assert!(ensure_init("version = 1\n[defaults\n").is_err());
        assert!(ensure_init("version = 2\n").is_err());
        assert!(ensure_init("bogus = true\n").is_err());
    }

    #[test]
    fn ignore_entry_appended_to_empty_and_unrelated() {
        assert_eq!(ensure_ignore_entry("", &cache()).unwrap(), "/.rustowcache/\n");
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
    fn ignore_entry_escapes_glob_characters() {
        for (name, entry) in [
            ("c[1]", "/c\\[1]/\n"),
            ("a*b", "/a\\*b/\n"),
            ("a?b", "/a\\?b/\n"),
            ("a\\b", "/a\\\\b/\n"),
            ("v/#x/!y z", "/v/#x/!y z/\n"),
        ] {
            let dir = RelPath::parse(name).unwrap();
            let out = ensure_ignore_entry("", &dir).unwrap();
            assert_eq!(out, entry, "{name}");
            assert_eq!(ensure_ignore_entry(&out, &dir), None, "{name}");
        }
        // Unescaped, "[1]" is a character class that does not match "c[1]".
        let out = ensure_ignore_entry("/c[1]/\n", &RelPath::parse("c[1]").unwrap()).unwrap();
        assert_eq!(out, "/c[1]/\n/c\\[1]/\n");
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
        assert!(out.contains("[[files]]\nsource = \".config/x.json\"\n"), "{out}");
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
    fn remove_file_from_inline_array() {
        let out = remove_file(
            "version = 1\nfiles = [{ source = \"a\" }, { source = \"b\" }]\n",
            &rel("a"),
        )
        .unwrap()
        .unwrap();
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(
            cfg.files.iter().map(RelPath::as_str).collect::<Vec<_>>(),
            ["b"],
            "{out}"
        );
    }

    #[test]
    fn remove_file_drops_only_that_entry() {
        let text = "# keep\nversion = 1\n[[files]]\nsource = \"a\"\n\n[[files]]\nsource = \"b\"\n";
        let out = remove_file(text, &rel("a")).unwrap().unwrap();
        let cfg = Config::parse(&out).unwrap();
        assert_eq!(cfg.files.iter().map(RelPath::as_str).collect::<Vec<_>>(), ["b"]);
        assert!(out.contains("# keep"), "{out}");
        assert_eq!(remove_file(&out, &rel("a")).unwrap(), None);
        let out = remove_file(&out, &rel("b")).unwrap().unwrap();
        assert!(Config::parse(&out).unwrap().files.is_empty(), "{out}");
    }

    #[test]
    fn rename_source_moves_files_and_rules_keeping_settings() {
        let text = "version = 1\n# c\n[[files]]\nsource = \"./a\"\n\n[[template_rules]]\nsource = \"t.tmpl\"\nmode = \"0600\"\n";
        let out = rename_source(text, &rel("a"), &rel("a.age")).unwrap().unwrap();
        assert!(out.contains("source = \"a.age\"") && out.contains("# c"), "{out}");
        let out = rename_source(&out, &rel("t.tmpl"), &rel("t.tmpl.age"))
            .unwrap()
            .unwrap();
        let cfg = Config::parse(&out).unwrap();
        assert!(cfg.files.contains(&rel("a.age")));
        assert_eq!(cfg.template_rules[&rel("t.tmpl.age")].mode, Some(0o600));
        assert_eq!(rename_source(&out, &rel("zzz"), &rel("zzz.age")).unwrap(), None);
        let inline = "version = 1\nfiles = [{ source = \"a\" }]\n";
        let out = rename_source(inline, &rel("a"), &rel("a.gpg")).unwrap().unwrap();
        assert!(Config::parse(&out).unwrap().files.contains(&rel("a.gpg")), "{out}");
    }

    #[test]
    fn rename_source_keeps_the_comments_of_the_entry() {
        let text = "version = 1\n[[template_rules]]\n# above\nsource = \"t.tmpl\" # after\nmode = \"0600\"\n\n\
                    [[files]]\n# above f\nsource = \"a\" # after f\n";
        let out = rename_source(text, &rel("t.tmpl"), &rel("t.tmpl.age"))
            .unwrap()
            .unwrap();
        let out = rename_source(&out, &rel("a"), &rel("a.age")).unwrap().unwrap();
        assert_eq!(
            out,
            text.replace("t.tmpl\"", "t.tmpl.age\"").replace("\"a\"", "\"a.age\"")
        );
        let inline = "version = 1\nfiles = [{source=\"a\"}] # f\n";
        let out = rename_source(inline, &rel("a"), &rel("a.age")).unwrap().unwrap();
        assert_eq!(out, inline.replace("\"a\"", "\"a.age\""));
    }

    #[test]
    fn add_and_remove_keep_crlf() {
        let text = "version = 1\r\n# mine\r\n[vars]\r\na = 1\r\n";
        let out = add_file(text, &rel("x")).unwrap().unwrap();
        assert!(out.starts_with(text) && out.contains("source = \"x\""), "{out:?}");
        assert_eq!(out.matches('\n').count(), out.matches("\r\n").count(), "{out:?}");
        let out = add_file(&out, &rel("y")).unwrap().unwrap();
        let out = remove_file(&out, &rel("x")).unwrap().unwrap();
        assert!(!out.contains("\"x\"") && out.contains("\"y\""), "{out:?}");
        assert_eq!(out.matches('\n').count(), out.matches("\r\n").count(), "{out:?}");
        assert_eq!(remove_file(&out, &rel("y")).unwrap().unwrap(), text);
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
