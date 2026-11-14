//! Shell completion. The installed script calls the binary on each TAB
//! with `RUSTOW_COMPLETE=<shell>` (clap_complete's dynamic engine). PATH
//! arguments complete repository-relative paths one directory at a time,
//! dotfiles included, ignored and reserved paths left out.

use std::ffi::{OsStr, OsString};
use std::fs::{DirEntry, ReadDir};
use std::io::{self, Write};
use std::path::Path;

use clap_complete::CompletionCandidate;
use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Shells, Zsh};

use crate::config::Backend;
use crate::ignore::Ignore;
use crate::paths::RelPath;
use crate::repo::{Context, Env, GlobalOpts};

/// The environment variable that turns a run into a completion request.
pub const VAR: &str = "RUSTOW_COMPLETE";

/// The shells of clap_complete; options complete only after a `-`.
pub const SHELLS: Shells<'static> = Shells(&[
    &DashFirst(Bash),
    &DashFirst(Elvish),
    &DashFirst(Fish),
    &DashFirst(Powershell),
    &DashFirst(Zsh),
]);

/// A shell whose candidates leave out options until the current word
/// starts with `-`. clap_complete offers them for an empty word too.
struct DashFirst<S>(S);

impl<S: EnvCompleter> EnvCompleter for DashFirst<S> {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn is(&self, name: &str) -> bool {
        self.0.is(name)
    }

    fn write_registration(
        &self,
        var: &str,
        name: &str,
        bin: &str,
        completer: &str,
        buf: &mut dyn Write,
    ) -> io::Result<()> {
        self.0.write_registration(var, name, bin, completer, buf)
    }

    fn write_complete(
        &self,
        cmd: &mut clap::Command,
        args: Vec<OsString>,
        current_dir: Option<&Path>,
        buf: &mut dyn Write,
    ) -> io::Result<()> {
        // The word the shell completes: at `_CLAP_COMPLETE_INDEX` (bash, elvish, zsh), else the last.
        let index = std::env::var("_CLAP_COMPLETE_INDEX").ok().and_then(|i| i.parse().ok());
        let current = args.get(index.unwrap_or(args.len().saturating_sub(1)));
        let dash = current.is_some_and(|w| w.as_encoded_bytes().starts_with(b"-"));
        let mut out = Vec::new();
        self.0.write_complete(cmd, args, current_dir, &mut out)?;
        if dash {
            return buf.write_all(&out);
        }
        // Each shell writes one entry per candidate, its value first, between `_CLAP_IFS` or newlines.
        let ifs = std::env::var("_CLAP_IFS").unwrap_or_else(|_| "\n".to_owned());
        let out = String::from_utf8_lossy(&out);
        let kept: Vec<&str> = out.split(ifs.as_str()).filter(|e| !e.starts_with('-')).collect();
        buf.write_all(kept.join(&ifs).as_bytes())
    }
}

/// Every source file and directory.
pub fn any(current: &OsStr) -> Vec<CompletionCandidate> {
    paths(current, |_| true)
}

/// Plaintext files, for `enc`.
pub fn plaintext(current: &OsStr) -> Vec<CompletionCandidate> {
    paths(current, |name| !is_encrypted(name))
}

/// `.age` and `.gpg` files, for `enc edit`.
pub fn encrypted(current: &OsStr) -> Vec<CompletionCandidate> {
    paths(current, is_encrypted)
}

fn is_encrypted(name: &str) -> bool {
    [Backend::Age, Backend::Gpg].iter().any(|b| name.ends_with(b.suffix()))
}

/// The entries of the repository directory that `current` is in, that
/// start with `current`. Without a repository, nothing.
fn paths(current: &OsStr, keep_file: fn(&str) -> bool) -> Vec<CompletionCandidate> {
    let Some(ctx) = open() else {
        return Vec::new();
    };
    let current = current.to_string_lossy();
    let dir = current.rfind('/').map_or("", |i| &current[..i]);
    let Some(entries) = read_repo_dir(&ctx.root, dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|entry| candidate(&ctx.ignore, dir, &entry, keep_file))
        .filter(|shown| shown.starts_with(current.as_ref()))
        .collect();
    out.sort();
    out.into_iter().map(CompletionCandidate::new).collect()
}

/// The repository, found as a command finds it (`$RUSTOW_REPO` or the current directory).
fn open() -> Option<Context> {
    let env = Env::from_process().ok()?;
    Context::open(&GlobalOpts::default(), &env).ok()
}

/// The entries of `dir`, a repository-relative directory ("" for the root).
fn read_repo_dir(root: &Path, dir: &str) -> Option<ReadDir> {
    let base = match dir {
        "" => root.to_path_buf(),
        _ => root.join(RelPath::parse(dir).ok()?.as_path()),
    };
    std::fs::read_dir(base).ok()
}

/// One entry as the shell shows it: a directory ends in `/`. Ignored and
/// reserved entries, and files that fail `keep_file`, are left out.
fn candidate(ignore: &Ignore, dir: &str, entry: &DirEntry, keep_file: fn(&str) -> bool) -> Option<String> {
    let name = entry.file_name().into_string().ok()?;
    let kind = entry.file_type().ok()?;
    let shown = if dir.is_empty() {
        name.clone()
    } else {
        format!("{dir}/{name}")
    };
    if ignore.is_ignored(&RelPath::parse(&shown).ok()?, kind.is_dir()) {
        None
    } else if kind.is_dir() {
        Some(format!("{shown}/"))
    } else {
        (kind.is_file() && keep_file(&name)).then_some(shown)
    }
}
