//! MiniJinja rendering. Undefined variables are errors, nothing is
//! auto-escaped, trailing newlines are kept, and templates cannot read
//! files or run programs.

use std::path::Path;

use minijinja::syntax::SyntaxConfig;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};
use serde_json::{Value, json};

use crate::config::{Metadata, Owner, parse_mode, parse_owner};
use crate::hash::Hash;

/// The built-in facts about this machine that templates and meta conditions read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub family: String,
    pub hostname: String,
    pub username: String,
}

impl Host {
    pub fn current() -> Host {
        let username = nix::unistd::User::from_uid(nix::unistd::geteuid())
            .ok()
            .flatten()
            .map(|u| u.name)
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_default();
        let hostname = nix::unistd::gethostname()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_default();
        Host {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            family: std::env::consts::FAMILY.into(),
            hostname,
            username,
        }
    }

    pub fn get(&self, fact: Fact) -> &str {
        match fact {
            Fact::Os => &self.os,
            Fact::Arch => &self.arch,
            Fact::Family => &self.family,
            Fact::Hostname => &self.hostname,
            Fact::Username => &self.username,
        }
    }
}

/// Template context: built-in platform values plus `vars` from `.rustow.toml`.
pub fn context(vars: &serde_json::Map<String, Value>, home_dir: &Path) -> Value {
    let host = Host::current();
    json!({
        "os": host.os,
        "arch": host.arch,
        "family": host.family,
        "hostname": host.hostname,
        "username": host.username,
        "uid": nix::unistd::geteuid().as_raw(),
        "gid": nix::unistd::getegid().as_raw(),
        "home_dir": home_dir.display().to_string(),
        "vars": vars,
    })
}

/// Renders `source`. Errors name the template and line, never values.
pub fn render(name: &str, source: &str, context: &Value) -> Result<String, String> {
    render_at(name, source, context, 0)
}

/// Renders a plaintext template: the meta block is removed first, and
/// error lines still count from the top of the file.
pub fn render_plain(name: &str, text: &str, context: &Value) -> Result<String, String> {
    let block = split_meta(text).map_err(|e| format!("template {name}, meta block {e}"))?;
    render_at(name, block.body, context, block.lines)
}

/// Renders `source`, which starts `offset` lines into the template file.
fn render_at(name: &str, source: &str, context: &Value, offset: usize) -> Result<String, String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    let syntax = SyntaxConfig::builder()
        .keep_trailing_newline(true)
        .build()
        .map_err(|e| e.to_string())?;
    env.set_syntax(syntax);
    let describe = |e: minijinja::Error| match e.line() {
        Some(line) => format!("template {name}, line {}: {}", line + offset, e.kind()),
        None => format!("template {name}: {}", e.kind()),
    };
    let template = env.template_from_named_str(name, source).map_err(describe)?;
    template.render(minijinja::value::Serde(context)).map_err(describe)
}

/// A host fact that a meta condition compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fact {
    Os,
    Arch,
    Family,
    Hostname,
    Username,
}

impl Fact {
    fn parse(key: &str) -> Option<Fact> {
        Some(match key {
            "os" => Fact::Os,
            "arch" => Fact::Arch,
            "family" => Fact::Family,
            "hostname" => Fact::Hostname,
            "username" => Fact::Username,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Fact::Os => "os",
            Fact::Arch => "arch",
            Fact::Family => "family",
            Fact::Hostname => "hostname",
            Fact::Username => "username",
        }
    }
}

/// `meta.<fact> == "<value>"`: the template deploys only when it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub fact: Fact,
    pub value: String,
}

impl std::fmt::Display for Condition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "meta.{} == \"{}\"", self.fact.name(), self.value)
    }
}

/// What a template's meta block says: metadata settings and deploy conditions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateMeta {
    pub settings: Metadata,
    pub conditions: Vec<Condition>,
}

impl TemplateMeta {
    pub fn has_settings(&self) -> bool {
        self.settings != Metadata::default()
    }

    /// The first condition that is false on `host`.
    pub fn failed(&self, host: &Host) -> Option<&Condition> {
        self.conditions.iter().find(|c| host.get(c.fact) != c.value)
    }
}

/// A template split into its meta block and the text to render.
#[derive(Debug)]
pub struct MetaBlock<'a> {
    pub meta: TemplateMeta,
    /// The text after the meta block.
    pub body: &'a str,
    /// Number of meta lines removed from the top.
    pub lines: usize,
}

/// Reads the meta block: the run of `{%- meta.<key> = | == <value> %}`
/// lines at the top of `text`. Errors start with "line N: ".
pub fn split_meta(text: &str) -> Result<MetaBlock<'_>, String> {
    let mut meta = TemplateMeta::default();
    let mut seen = Vec::new();
    let mut body = text;
    let mut lines = 0;
    while !body.is_empty() {
        let (line, after) = body.split_once('\n').unwrap_or((body, ""));
        let Some(stmt) = meta_line(line) else { break };
        lines += 1;
        if seen.contains(&stmt.key) {
            return Err(format!("line {lines}: meta.{} is set twice", stmt.key));
        }
        seen.push(stmt.key);
        apply(&mut meta, &stmt).map_err(|e| format!("line {lines}: {e}"))?;
        body = after;
    }
    Ok(MetaBlock { meta, body, lines })
}

enum MetaValue<'a> {
    Text(&'a str),
    Number(&'a str),
}

struct Stmt<'a> {
    key: &'a str,
    test: bool,
    value: MetaValue<'a>,
}

/// One meta line, or None when the line is not one.
fn meta_line(line: &str) -> Option<Stmt<'_>> {
    let s = line.trim_end_matches([' ', '\t', '\r']).trim_start_matches([' ', '\t']);
    let s = s.strip_prefix("{%")?;
    let s = s.strip_prefix('-').unwrap_or(s).trim_start();
    let s = s.strip_prefix("meta.")?;
    let key_len = s.bytes().take_while(u8::is_ascii_lowercase).count();
    let (key, s) = s.split_at(key_len);
    let (test, s) = operator(s.trim_start())?;
    let (value, s) = meta_value(s.trim_start())?;
    let s = s.trim_start();
    let s = s.strip_prefix('-').unwrap_or(s);
    (!key.is_empty() && s == "%}").then_some(Stmt { key, test, value })
}

/// `==` (a test, true) or `=` (a setting, false), and the text after it.
fn operator(s: &str) -> Option<(bool, &str)> {
    match s.strip_prefix("==") {
        Some(rest) => Some((true, rest)),
        None => Some((false, s.strip_prefix('=')?)),
    }
}

/// A double-quoted string without `"` or `\` inside, or a run of digits; and the text after it.
fn meta_value(s: &str) -> Option<(MetaValue<'_>, &str)> {
    if let Some(rest) = s.strip_prefix('"') {
        let (inner, rest) = rest.split_once('"')?;
        return (!inner.contains('\\')).then_some((MetaValue::Text(inner), rest));
    }
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    (n > 0).then_some((MetaValue::Number(&s[..n]), &s[n..]))
}

/// Adds one meta statement to `meta`; the error is without the line.
fn apply(meta: &mut TemplateMeta, stmt: &Stmt) -> Result<(), String> {
    match Fact::parse(stmt.key) {
        Some(fact) => add_condition(meta, fact, stmt),
        None => add_setting(&mut meta.settings, stmt),
    }
}

fn add_condition(meta: &mut TemplateMeta, fact: Fact, stmt: &Stmt) -> Result<(), String> {
    if !stmt.test {
        return Err(format!("meta.{} is a condition; use ==", stmt.key));
    }
    let value = text_value(stmt)?.to_owned();
    meta.conditions.push(Condition { fact, value });
    Ok(())
}

fn add_setting(s: &mut Metadata, stmt: &Stmt) -> Result<(), String> {
    let key = stmt.key;
    if stmt.test {
        return Err(format!("meta.{key} is a setting; use ="));
    }
    let (slot, id_key, name_key) = match key {
        "mode" => {
            s.mode = Some(parse_mode(text_value(stmt)?)?);
            return Ok(());
        }
        "uid" | "owner" => (&mut s.owner, "uid", "owner"),
        "gid" | "group" => (&mut s.group, "gid", "group"),
        _ => return Err(format!("unknown meta key 'meta.{key}'")),
    };
    *slot = Some(owner_value(stmt, slot.is_some(), id_key, name_key)?);
    Ok(())
}

/// The owner or group of `meta.<id_key>` (a number) or `meta.<name_key>` (a name).
fn owner_value(stmt: &Stmt, already: bool, id_key: &str, name_key: &str) -> Result<Owner, String> {
    if already {
        return Err(format!("meta.{id_key} and meta.{name_key} cannot both be set"));
    }
    if stmt.key == id_key {
        let MetaValue::Number(n) = stmt.value else {
            return Err(format!("meta.{id_key} needs a whole number"));
        };
        return n
            .parse()
            .map(Owner::Id)
            .map_err(|_| format!("meta.{id_key} {n} is too large"));
    }
    parse_owner(name_key, text_value(stmt)?)
}

fn text_value<'a>(stmt: &Stmt<'a>) -> Result<&'a str, String> {
    match stmt.value {
        MetaValue::Text(t) => Ok(t),
        MetaValue::Number(_) => Err(format!("meta.{} needs a quoted string", stmt.key)),
    }
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
            json!({ "editor": "nvim", "git": { "user_name": "Example User" } })
                .as_object()
                .unwrap(),
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
        let c = context(json!({ "v": "<a&\"b\">" }).as_object().unwrap(), Path::new("/h"));
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

    fn host() -> Host {
        Host {
            os: "linux".into(),
            arch: "x86_64".into(),
            family: "unix".into(),
            hostname: "laptop".into(),
            username: "me".into(),
        }
    }

    #[test]
    fn meta_block_is_parsed_and_stripped() {
        let t = "{%- meta.os == \"linux\" %}\n{%- meta.uid = 1001 %}\n{%- meta.group = \"wheel\" %}\n\
                 {%- meta.mode = \"0600\" %}\nbody\n";
        let b = split_meta(t).unwrap();
        assert_eq!(b.body, "body\n");
        assert_eq!(b.lines, 4);
        assert_eq!(
            b.meta.settings,
            Metadata {
                mode: Some(0o600),
                owner: Some(Owner::Id(1001)),
                group: Some(Owner::Name("wheel".into()))
            }
        );
        assert_eq!(
            b.meta.conditions,
            vec![Condition {
                fact: Fact::Os,
                value: "linux".into()
            }]
        );
        assert!(b.meta.has_settings());
        assert!(b.meta.failed(&host()).is_none());
    }

    #[test]
    fn false_condition_is_reported() {
        let b = split_meta("{%- meta.hostname == \"pc\" %}\nx").unwrap();
        assert!(!b.meta.has_settings());
        assert_eq!(b.meta.failed(&host()).unwrap().to_string(), "meta.hostname == \"pc\"");
    }

    #[test]
    fn every_fact_is_compared() {
        for (fact, value) in [
            ("os", "linux"),
            ("arch", "x86_64"),
            ("family", "unix"),
            ("hostname", "laptop"),
            ("username", "me"),
        ] {
            let ok = format!("{{% meta.{fact} == \"{value}\" %}}\n");
            assert!(split_meta(&ok).unwrap().meta.failed(&host()).is_none(), "{fact}");
            let no = format!("{{% meta.{fact} == \"other\" %}}\n");
            assert!(split_meta(&no).unwrap().meta.failed(&host()).is_some(), "{fact}");
        }
    }

    #[test]
    fn no_meta_block_leaves_text_alone() {
        let t = "{%- if os == \"linux\" %}\na\n{%- endif %}\n";
        let b = split_meta(t).unwrap();
        assert_eq!((b.body, b.lines), (t, 0));
        assert_eq!(b.meta, TemplateMeta::default());
    }

    #[test]
    fn crlf_and_trailing_spaces_are_accepted() {
        let b = split_meta("{% meta.os == \"linux\" -%}  \r\nbody\r\n").unwrap();
        assert_eq!((b.body, b.lines), ("body\r\n", 1));
    }

    #[test]
    fn meta_block_may_be_the_whole_file() {
        let b = split_meta("{%- meta.uid = 0 %}").unwrap();
        assert_eq!((b.body, b.lines), ("", 1));
    }

    #[test]
    fn owner_names_the_owner_like_template_rules() {
        let b = split_meta("{%- meta.owner = \"root\" %}\n").unwrap();
        assert_eq!(b.meta.settings.owner, Some(Owner::Name("root".into())));
        assert_eq!(
            split_meta("{%- meta.uid = 0 %}\n{%- meta.owner = \"root\" %}\n").unwrap_err(),
            "line 2: meta.uid and meta.owner cannot both be set"
        );
        assert_eq!(
            split_meta("{%- meta.user = \"root\" %}\n").unwrap_err(),
            "line 1: unknown meta key 'meta.user'"
        );
    }

    #[test]
    fn meta_errors_name_the_line() {
        for (text, want) in [
            ("{%- meta.color = \"red\" %}\n", "line 1: "),
            ("{%- meta.os = \"linux\" %}\n", "line 1: "),
            ("{%- meta.uid == 1 %}\n", "line 1: "),
            ("{%- meta.uid = \"root\" %}\n", "line 1: "),
            ("{%- meta.owner = 7 %}\n", "line 1: "),
            (
                "{%- meta.os == \"linux\" %}\n{%- meta.uid = 1 %}\n{%- meta.owner = \"me\" %}\n",
                "line 3: ",
            ),
            ("{%- meta.gid = 1 %}\n{%- meta.group = \"g\" %}\n", "line 2: "),
            ("{%- meta.mode = \"0600\" %}\n{%- meta.mode = \"0644\" %}\n", "line 2: "),
            ("{%- meta.os == \"linux\" %}\n{%- meta.os == \"macos\" %}\n", "line 2: "),
            ("{%- meta.mode = \"9999\" %}\n", "line 1: "),
        ] {
            let e = split_meta(text).err().unwrap_or_else(|| panic!("{text:?} must fail"));
            assert!(e.starts_with(want), "{text:?} -> {e}");
        }
    }

    #[test]
    fn render_plain_strips_block_and_offsets_error_lines() {
        let c = ctx();
        assert_eq!(
            render_plain("t", "{%- meta.os == \"linux\" %}\ne={{ vars.editor }}\n", &c).unwrap(),
            "e=nvim\n"
        );
        let e = render_plain(
            "t",
            "{%- meta.os == \"linux\" %}\n{%- meta.uid = 1 %}\n{{ missing }}\n",
            &c,
        )
        .unwrap_err();
        assert!(e.contains("line 3"), "{e}");
    }

    #[test]
    fn host_matches_the_context() {
        let h = Host::current();
        let c = ctx();
        assert_eq!(
            (c["os"].as_str(), c["hostname"].as_str()),
            (Some(h.os.as_str()), Some(h.hostname.as_str()))
        );
        assert_eq!(c["username"].as_str(), Some(h.username.as_str()));
    }
}
