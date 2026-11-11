//! Conservative text edits of `.rustow.toml` and ignore files. Edits keep
//! comments, formatting and line endings; `None` means "already right".

use ::ignore::gitignore::GitignoreBuilder;
use toml_edit::DocumentMut;

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

    #[test]
    fn nested_custom_cache_dir() {
        let dir = RelPath::parse("var/cache").unwrap();
        assert_eq!(ensure_ignore_entry("var/\n", &dir), None);
        assert_eq!(ensure_ignore_entry("", &dir).unwrap(), "/var/cache/\n");
    }
}
