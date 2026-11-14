//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::ArgValueCompleter;

use crate::execute::Policy;

#[derive(Debug, Parser)]
#[command(
    name = "rustow",
    version,
    about = "Flat-repository dotfiles manager: symlinks, copies, templates and encrypted files",
    long_about = "Flat-repository dotfiles manager. The repository root mirrors your home \
                  directory; rustowroot/ mirrors /. Without a command, rustow runs `apply`.\n\n\
                  Exit codes: 0 success, 1 differences found, 2 usage or configuration error, \
                  3 conflicts, 4 runtime or partial failure (also a first SIGINT, SIGTERM or SIGHUP \
                  while files change), 130 a second such signal, 141 a closed output pipe. A signal \
                  at another time ends rustow with that signal (the shell shows 128 + its number)."
)]
pub struct Cli {
    /// The conflict options of bare `rustow` (`apply`); a command takes its own.
    #[command(flatten)]
    pub conflict: Conflict,
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Option<Command>,
}

// Each command reads these: `--target` is also the way to run under sudo.
const PLACE: &[&str] = &["repo", "target", "root_target", "json", "quiet"];
// Item lines: unchanged ones (`-v`) and colored verbs.
const ITEMS: &[&str] = &["verbose", "no_color"];
const DRY: &[&str] = &["dry_run"];
// Commands that decrypt or encrypt.
const UNLOCK: &[&str] = &["interactive"];
const DEPLOY: &[&[&str]] = &[PLACE, ITEMS, DRY, UNLOCK];

/// The global options that each command takes, by its path ("" is bare
/// `rustow`). Help and completion hide the others there, and `try_parse_from`
/// refuses them. A command that is not here takes none.
const TAKES: &[(&str, &[&[&str]])] = &[
    ("", DEPLOY),
    ("apply", DEPLOY),
    ("file apply", DEPLOY),
    ("template apply", DEPLOY),
    ("link", &[PLACE, ITEMS, DRY]),
    ("unlink", &[PLACE, ITEMS, DRY]),
    ("undeploy", &[PLACE, ITEMS, DRY]),
    ("init", &[PLACE, &["verbose"], DRY]),
    ("file add", &[PLACE, DRY]),
    ("file remove", &[PLACE, DRY]),
    ("cache clean", &[PLACE, DRY]),
    ("state import", &[PLACE, DRY]),
    ("state adopt", &[PLACE, DRY]),
    ("status", &[PLACE, &["verbose"], UNLOCK]),
    ("diff", &[PLACE, UNLOCK]),
    ("file diff", &[PLACE, UNLOCK]),
    ("enc", &[PLACE, UNLOCK]),
    ("enc edit", &[PLACE, UNLOCK]),
    ("file list", &[PLACE]),
    ("template list", &[PLACE]),
    ("check", &[PLACE]),
    ("doctor", &[PLACE]),
    ("state export", &[PLACE]),
];

const GLOBAL: &[&str] = &[
    "repo",
    "target",
    "root_target",
    "dry_run",
    "verbose",
    "quiet",
    "json",
    "no_color",
    "interactive",
];

/// The global options that `path` or a command below it takes.
fn shown(path: &str) -> impl Iterator<Item = &'static str> + '_ {
    let below = move |p: &str| path.is_empty() || p == path || p.strip_prefix(path).is_some_and(|r| r.starts_with(' '));
    (TAKES.iter().filter(move |(p, _)| below(p))).flat_map(|(_, groups)| groups.iter().copied().flatten().copied())
}

fn takes(path: &str, id: &str) -> bool {
    TAKES
        .iter()
        .any(|(p, groups)| *p == path && groups.iter().any(|g| g.contains(&id)))
}

/// The command line, where each command shows only the global options it takes.
pub fn command() -> clap::Command {
    let cmd = <Cli as clap::CommandFactory>::command();
    let globals: Vec<clap::Arg> = cmd.get_arguments().filter(|a| a.is_global_set()).cloned().collect();
    limit(cmd, "", &globals)
}

/// Gives each command below `cmd` its own copy of each global option,
/// hidden where it does not take it. clap copies a global option only into
/// a command that lacks it, and a change after `build` breaks its lookup.
fn limit(mut cmd: clap::Command, path: &str, globals: &[clap::Arg]) -> clap::Command {
    let names: Vec<String> = cmd.get_subcommands().map(|s| s.get_name().to_owned()).collect();
    for name in names {
        let below = if path.is_empty() {
            name.clone()
        } else {
            format!("{path} {name}")
        };
        let shown: Vec<&str> = shown(&below).collect();
        cmd = cmd.mut_subcommand(&name, |mut sub| {
            for arg in globals {
                sub = sub.arg(arg.clone().hide(!shown.contains(&arg.get_id().as_str())));
            }
            limit(sub, &below, globals)
        });
    }
    cmd
}

/// Parses the command line; a global option that the command does not take
/// is an error, as an unknown option is.
pub fn try_parse_from<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::FromArgMatches;
    let mut cmd = command();
    let matches = cmd.try_get_matches_from_mut(args)?;
    let (mut path, mut leaf, mut sub_cmd) = (Vec::new(), &matches, &mut cmd);
    while let Some((name, sub)) = leaf.subcommand() {
        path.push(name);
        leaf = sub;
        sub_cmd = sub_cmd.find_subcommand_mut(name).expect("clap matched this command");
    }
    let path = path.join(" ");
    let given = |id: &str| leaf.value_source(id) == Some(clap::parser::ValueSource::CommandLine);
    if let Some(id) = GLOBAL.iter().find(|id| given(id) && !takes(&path, id)) {
        let message = format!("`rustow {path}` does not take --{}", id.replace('_', "-"));
        return Err(sub_cmd.error(clap::error::ErrorKind::UnknownArgument, message));
    }
    Cli::from_arg_matches(&matches)
}

/// A heading of their own puts the options of a command first, in help and completion.
#[derive(Debug, Args)]
#[command(next_help_heading = "Global options")]
pub struct Global {
    /// Repository root (default: $RUSTOW_REPO, else the nearest directory with rustow.toml or .rustowignore; `init` uses the current directory)
    #[arg(short, long, global = true, value_name = "DIR")]
    pub repo: Option<PathBuf>,
    /// Target for home sources (default: defaults.target, normally ~)
    #[arg(short, long, global = true, value_name = "DIR")]
    pub target: Option<PathBuf>,
    /// Target for rustowroot/ sources (default: defaults.root_target, normally /)
    #[arg(short = 'R', long, global = true, value_name = "DIR")]
    pub root_target: Option<PathBuf>,
    /// Show what would change; change nothing
    #[arg(short = 'n', long, global = true)]
    pub dry_run: bool,
    /// Also list unchanged items
    #[arg(short, long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,
    /// Print only conflicts and errors
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Machine-readable JSON output
    #[arg(short, long, global = true)]
    pub json: bool,
    /// Disable colors (also: NO_COLOR)
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Allow passphrase prompts (pinentry, terminal)
    #[arg(short, long, global = true)]
    pub interactive: bool,
}

/// What a deploy does with a destination that it may not change.
#[derive(Debug, Default, Args)]
pub struct Conflict {
    /// Resolve conflicts by moving the existing destination into the backup directory
    #[arg(short, long, conflicts_with_all = ["force", "on_conflict"])]
    pub backup: bool,
    /// Resolve conflicts by replacing an existing file or symlink (never a directory)
    #[arg(short, long, conflicts_with = "on_conflict")]
    pub force: bool,
    /// Conflict policy (default: skip)
    #[arg(short = 'c', long, value_enum, value_name = "POLICY")]
    pub on_conflict: Option<Policy>,
}

impl Conflict {
    pub fn is_set(&self) -> bool {
        self.backup || self.force || self.on_conflict.is_some()
    }

    pub fn policy(&self) -> Policy {
        match (self.on_conflict, self.backup, self.force) {
            (Some(p), _, _) => p,
            (None, true, _) => Policy::Backup,
            (None, false, true) => Policy::Overwrite,
            (None, false, false) => Policy::Skip,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Deploy all eligible sources: links, copies, templates, encrypted files (default)
    Apply(DeployArgs),
    /// Deploy ordinary plaintext symlinks only
    Link(DeployArgs),
    /// Remove rustow-managed ordinary symlinks
    Unlink(RemovalArgs),
    /// Prepare a new or existing repository (config, ignore files, private cache); deploys nothing
    Init,
    /// Manage sources deployed by copying instead of symlinking
    #[command(subcommand)]
    File(FileCommand),
    /// Render and deploy templates
    #[command(subcommand)]
    Template(TemplateCommand),
    /// Show differences between deployed and desired content (may print secrets)
    Diff(Paths),
    /// Show the state of each destination (never prints content)
    Status(StatusArgs),
    /// Validate the repository offline (configuration, ignore rules, collisions, templates)
    Check,
    /// Report on the environment: directories, permissions, GnuPG, identities
    Doctor,
    /// Remove rustow-managed destinations of any mode (verified first)
    Undeploy(UndeployArgs),
    /// Manage the repository cache
    #[command(subcommand)]
    Cache(CacheCommand),
    /// Export or import the registrations of `file add` and copy-mode `link`
    #[command(subcommand)]
    State(StateCommand),
    /// Encrypt a repository source in place (age by default), or `enc edit` an encrypted source
    Enc(EncArgs),
    /// Internal: writes system files as root for a rustow run that started it through sudo
    #[command(name = "__sudo-helper", hide = true)]
    SudoHelper,
    /// Print a shell completion script
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum FileCommand {
    /// Register sources for copy deployment; deploys nothing
    Add(RequiredPaths),
    /// Remove copy registrations; deployed copies stay in place
    Remove(RequiredPaths),
    /// List registered sources
    List,
    /// Deploy registered copies only
    Apply(DeployArgs),
    /// Diff registered copies against their destinations (may print secrets)
    Diff(Paths),
}

#[derive(Debug, Subcommand)]
pub enum TemplateCommand {
    /// List template sources and how they deploy
    List,
    /// Deploy templates only (including encrypted templates)
    Apply(DeployArgs),
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct EncArgs {
    #[command(subcommand)]
    pub command: Option<EncCommand>,
    /// Backend for this encryption only (default: encryption.backend)
    #[arg(short = 'B', long, value_enum)]
    pub backend: Option<crate::config::Backend>,
    /// Repository-relative plaintext source (a file named "edit" is written ./edit)
    #[arg(value_name = "PATH", required = true, add = ArgValueCompleter::new(crate::complete::plaintext))]
    pub path: Option<String>,
    /// Arguments after PATH, refused by `app`: clap would call one a subcommand.
    #[arg(hide = true)]
    pub extra: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum EncCommand {
    /// Decrypt into a private workspace, run $VISUAL or $EDITOR, re-encrypt with the same backend
    Edit {
        /// Re-encrypt even when content and recipients look unchanged
        #[arg(short = 'e', long)]
        reencrypt: bool,
        /// Repository-relative encrypted source (*.age or *.gpg)
        #[arg(value_name = "PATH", add = ArgValueCompleter::new(crate::complete::encrypted))]
        path: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Remove cache entries no managed symlink uses, and stale edit workspaces
    Clean,
}

#[derive(Debug, Subcommand)]
pub enum StateCommand {
    /// Print the registrations of this machine as JSON (or write them to FILE; "-" prints them)
    Export {
        /// Replace FILE when it exists
        #[arg(short, long)]
        force: bool,
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Add the registrations in FILE ("-" for stdin) to this machine
    Import {
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Take over the state of this repository from its path before a move
    Adopt {
        #[arg(value_name = "OLD_ROOT")]
        old_root: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Also list ignored repository paths
    #[arg(short = 'I', long)]
    pub ignored: bool,
    #[command(flatten)]
    pub paths: Paths,
}

#[derive(Debug, Args)]
pub struct UndeployArgs {
    /// Do not ask before removing everything (required without a terminal)
    #[arg(short, long)]
    pub yes: bool,
    #[command(flatten)]
    pub removal: RemovalArgs,
}

#[derive(Debug, Default, Args)]
pub struct DeployArgs {
    #[command(flatten)]
    pub conflict: Conflict,
    #[command(flatten)]
    pub paths: Paths,
}

/// `unlink` and `undeploy` never remove a destination that changed.
#[derive(Debug, Args)]
pub struct RemovalArgs {
    /// Conflict policy (default: skip); a changed destination always stays
    #[arg(short = 'c', long, value_enum, value_name = "POLICY")]
    pub on_conflict: Option<RemovalPolicy>,
    #[command(flatten)]
    pub paths: Paths,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum RemovalPolicy {
    /// Remove nothing at all when any item conflicts.
    Abort,
    /// Leave changed destinations as they are; remove the rest (default).
    Skip,
}

impl RemovalArgs {
    pub fn abort(&self) -> bool {
        self.on_conflict == Some(RemovalPolicy::Abort)
    }
}

#[derive(Debug, Default, Args)]
pub struct Paths {
    /// Repository-relative files or directories (default: all)
    #[arg(value_name = "PATH", add = ArgValueCompleter::new(crate::complete::any))]
    pub paths: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RequiredPaths {
    /// Repository-relative source files
    #[arg(value_name = "PATH", required = true, add = ArgValueCompleter::new(crate::complete::any))]
    pub paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{TAKES, command, try_parse_from};

    #[test]
    fn each_short_option_parses_as_its_long_option() {
        command().debug_assert();
        let parsed = |args: &[&str]| format!("{:?}", try_parse_from([&["rustow"], args].concat()));
        for (short, long) in [
            (
                &["-r", "d", "-t", "h", "-R", "/r", "-j", "-i", "apply"][..],
                &[
                    "--repo",
                    "d",
                    "--target",
                    "h",
                    "--root-target",
                    "/r",
                    "--json",
                    "--interactive",
                    "apply",
                ][..],
            ),
            (&["-b"], &["--backup"]),
            (&["apply", "-f"], &["apply", "--force"]),
            (&["link", "-c", "abort"], &["link", "--on-conflict", "abort"]),
            (&["unlink", "-c", "skip"], &["unlink", "--on-conflict", "skip"]),
            (&["undeploy", "-c", "abort"], &["undeploy", "--on-conflict", "abort"]),
            (&["state", "export", "-f", "x"], &["state", "export", "--force", "x"]),
            (&["status", "-I"], &["status", "--ignored"]),
            (&["enc", "-B", "age", "p"], &["enc", "--backend", "age", "p"]),
            (&["enc", "edit", "-e", "p"], &["enc", "edit", "--reencrypt", "p"]),
        ] {
            let got = parsed(short);
            assert!(got.starts_with("Ok("), "{short:?}: {got}");
            assert_eq!(got, parsed(long), "{short:?}");
        }
    }

    #[test]
    fn each_command_in_the_option_table_exists() {
        for (path, _) in TAKES {
            let mut cmd = command();
            for name in path.split_whitespace() {
                cmd = cmd.find_subcommand(name).unwrap_or_else(|| panic!("{path:?}")).clone();
            }
        }
    }
}
