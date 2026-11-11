//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::execute::Policy;

#[derive(Debug, Parser)]
#[command(
    name = "rustow",
    version,
    about = "Flat-repository dotfiles manager: symlinks, copies, templates and encrypted files",
    long_about = "Flat-repository dotfiles manager. The repository root mirrors your home \
                  directory; .rustowroot/ mirrors /. Without a command, rustow runs `apply`.\n\n\
                  Exit codes: 0 success, 1 differences found, 2 usage or configuration error, \
                  3 conflicts, 4 runtime or partial failure."
)]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args)]
pub struct Global {
    /// Repository root (default: $RUSTOW_REPO, else the nearest directory with .rustow.toml or .rustowignore, else the current directory)
    #[arg(long, global = true, value_name = "DIR")]
    pub repo: Option<PathBuf>,
    /// Target for home sources (default: defaults.target, normally ~)
    #[arg(long, global = true, value_name = "DIR")]
    pub target: Option<PathBuf>,
    /// Target for .rustowroot/ sources (default: defaults.root_target, normally /)
    #[arg(long, global = true, value_name = "DIR")]
    pub root_target: Option<PathBuf>,
    /// Show what would change; change nothing
    #[arg(short = 'n', long, global = true)]
    pub dry_run: bool,
    /// Resolve conflicts by moving the existing destination into the backup directory
    #[arg(long, global = true, conflicts_with_all = ["force", "on_conflict"])]
    pub backup: bool,
    /// Resolve conflicts by replacing an existing file or symlink (never a directory)
    #[arg(long, global = true, conflicts_with = "on_conflict")]
    pub force: bool,
    /// Conflict policy
    #[arg(long, global = true, value_enum, value_name = "POLICY")]
    pub on_conflict: Option<Policy>,
    /// Also list unchanged items
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,
    /// Print only conflicts and errors
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    pub json: bool,
    /// Disable colors (also: NO_COLOR)
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Allow passphrase prompts (pinentry, terminal)
    #[arg(long, global = true)]
    pub interactive: bool,
}

impl Global {
    pub fn policy(&self) -> Policy {
        match (self.on_conflict, self.backup, self.force) {
            (Some(p), _, _) => p,
            (None, true, _) => Policy::Backup,
            (None, false, true) => Policy::Overwrite,
            (None, false, false) => Policy::Abort,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Deploy all eligible sources: links, copies, templates, encrypted files (default)
    Apply(Paths),
    /// Deploy ordinary plaintext symlinks only
    Link(Paths),
    /// Remove rustow-managed ordinary symlinks
    Unlink(Paths),
}

#[derive(Debug, Args)]
pub struct Paths {
    /// Repository-relative files or directories (default: all)
    #[arg(value_name = "PATH")]
    pub paths: Vec<String>,
}
