//! MiniJinja rendering. Undefined variables are errors, nothing is
//! auto-escaped, trailing newlines are kept, and templates cannot read
//! files or run programs.

use std::path::Path;

use minijinja::syntax::SyntaxConfig;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde_json::{Value, json};

use crate::hash::Hash;

/// Template context: built-in platform values plus `vars` from `.rustow.toml`.
pub fn context(vars: &Value, home_dir: &Path) -> Value {
    let uid = nix::unistd::geteuid();
    let username = nix::unistd::User::from_uid(uid)
        .ok()
        .flatten()
        .map(|u| u.name)
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default();
    let hostname = nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_default();
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
        "hostname": hostname,
        "username": username,
        "uid": uid.as_raw(),
        "gid": nix::unistd::getegid().as_raw(),
        "home_dir": home_dir.display().to_string(),
        "vars": vars,
    })
}

/// Renders `source`. Errors name the template and line, never values.
pub fn render(name: &str, source: &str, context: &Value) -> Result<String, String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    let syntax = SyntaxConfig::builder()
        .keep_trailing_newline(true)
        .build()
        .map_err(|e| e.to_string())?;
    env.set_syntax(syntax);
    let describe = |e: minijinja::Error| match e.line() {
        Some(line) => format!("template {name}, line {line}: {}", e.kind()),
        None => format!("template {name}: {}", e.kind()),
    };
    let template = env.template_from_named_str(name, source).map_err(describe)?;
    template.render(minijinja::value::Serde(context)).map_err(describe)
}

/// Identity of one rendering: changes when the template or any context value changes.
pub fn render_id(source: &[u8], context: &Value) -> Hash {
    Hash::of_parts(&[source, context.to_string().as_bytes()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Value {
        context(
            &json!({ "editor": "nvim", "git": { "user_name": "Example User" } }),
            Path::new("/home/u"),
        )
    }

    #[test]
    fn renders_vars_builtins_and_keeps_trailing_newline() {
        let out = render(
            "t",
            "{{ vars.git.user_name }} {{ vars.editor }} {{ home_dir }}\n",
            &ctx(),
        )
        .unwrap();
        assert_eq!(out, "Example User nvim /home/u\n");
    }

    #[test]
    fn builtins_are_present() {
        let c = ctx();
        assert_eq!(c["os"], std::env::consts::OS);
        assert_eq!(c["arch"], std::env::consts::ARCH);
        for key in ["hostname", "username"] {
            assert!(c[key].as_str().is_some_and(|s| !s.is_empty()), "{key}: {c}");
        }
        assert!(c["uid"].is_u64() && c["gid"].is_u64());
    }

    #[test]
    fn os_conditionals_work() {
        let src = "{% if os == \"linux\" %}cache{% elif os == \"macos\" %}osxkeychain{% endif %}";
        let out = render("t", src, &ctx()).unwrap();
        assert_eq!(
            out,
            if cfg!(target_os = "macos") {
                "osxkeychain"
            } else {
                "cache"
            }
        );
    }

    #[test]
    fn missing_variable_is_error_naming_template() {
        let e = render(".gitconfig.tmpl", "a\n{{ vars.nope.deeper }}\n", &ctx()).unwrap_err();
        assert!(e.contains(".gitconfig.tmpl") && e.contains("line 2"), "{e}");
        assert!(render("t", "{% if vars.nope %}x{% endif %}", &ctx()).is_err());
        assert_eq!(
            render("t", "{% if vars.nope is defined %}x{% endif %}", &ctx()).unwrap(),
            ""
        );
    }

    #[test]
    fn syntax_error_is_reported() {
        let e = render("bad.tmpl", "{% if %}", &ctx()).unwrap_err();
        assert!(e.contains("bad.tmpl"), "{e}");
    }

    #[test]
    fn nothing_is_escaped() {
        let c = context(&json!({ "v": "<a&\"b\">" }), Path::new("/h"));
        assert_eq!(render("x.html", "{{ vars.v }}", &c).unwrap(), "<a&\"b\">");
        assert_eq!(render("x.json", "{{ vars.v }}", &c).unwrap(), "<a&\"b\">");
    }

    #[test]
    fn render_id_tracks_source_and_context() {
        let a = render_id(b"x", &json!({"a": 1}));
        assert_eq!(a, render_id(b"x", &json!({"a": 1})));
        assert_ne!(a, render_id(b"y", &json!({"a": 1})));
        assert_ne!(a, render_id(b"x", &json!({"a": 2})));
    }
}
