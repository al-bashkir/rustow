//! Command dispatch: builds the context and runs one command.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use crate::cli::{
    CacheCommand, Cli, Command, EncArgs, EncCommand, FileCommand, Global, Paths, StateCommand, StatusArgs,
    TemplateCommand,
};
use crate::config::DefaultMode;
use crate::error::{Error, Result};
use crate::execute::{self, Action, ItemReport, Policy, Run};
use crate::paths::{RelPath, Scope};
use crate::plan::{self, Plan};
use crate::repo::{Context, Env, GlobalOpts};
use crate::report::{Listed, Printer, RunKind};
use crate::source::{Discovery, Mode, Selector, Source};
use crate::state::{self, Lock, Registration, State};
use crate::{files, fsops, inspect};

/// Runs the parsed command and returns the process exit code.
pub fn run(cli: Cli) -> Result<i32> {
    let env = Env::from_process()?;
    refuse_sudo(&cli, env.sudo_user.as_deref())?;
    let g = &cli.global;
    let mut printer = Printer::new(g.json, g.quiet, g.verbose, g.no_color);
    let p = &mut printer;
    match cli.command.unwrap_or(Command::Apply(Paths::default())) {
        Command::Apply(a) => deploy(g, &env, p, &a.paths, Selector::All),
        Command::Link(a) => deploy(g, &env, p, &a.paths, Selector::Link),
        Command::Unlink(a) => undeploy(g, &env, p, &a.paths, Selector::Link, false),
        Command::Init => init_cmd(g, &env, p),
        Command::File(c) => file_cmd(g, &env, p, c),
        Command::Template(c) => template_cmd(g, &env, p, c),
        Command::Diff(a) => diff_cmd(g, &env, p, &a.paths, Selector::All),
        Command::Status(a) => status_cmd(g, &env, p, &a),
        Command::Check => check_cmd(g, &env, p),
        Command::Doctor => crate::doctor::doctor(&open(g, &env, p)?, p),
        Command::Undeploy(a) => {
            let ask = a.paths.paths.is_empty() && !a.yes;
            undeploy(g, &env, p, &a.paths.paths, Selector::All, ask)
        }
        Command::Cache(CacheCommand::Clean) => cache_clean(g, &env, p),
        Command::State(c) => state_cmd(g, &env, p, c),
        Command::Enc(e) => enc_cmd(g, &env, p, e),
        Command::Completions { shell } => completions(shell),
        Command::SudoHelper => crate::sudo::serve(),
    }
}

/// Catches SIGXFSZ: past the file size limit (`ulimit -f`), a write then
/// fails with "File too large" instead of a signal that kills rustow in
/// the middle of a write. `main` calls it before `run`, also for the helper.
pub fn catch_file_size_signal() -> Result<()> {
    signal_hook::flag::register(signal_hook::consts::SIGXFSZ, Arc::new(AtomicBool::new(false)))
        .map(drop)
        .map_err(|e| Error::Failed(format!("install signal handler: {e}")))
}

/// Under `sudo`, HOME and the state directory are root's: rustow would put
/// the user's dotfiles into root's home. An explicit `--target` is a real
/// root setup; `completions` touches no files, and the sudo helper is
/// started through sudo on purpose.
fn refuse_sudo(cli: &Cli, sudo_user: Option<&str>) -> Result<()> {
    let exempt = matches!(cli.command, Some(Command::Completions { .. } | Command::SudoHelper));
    match sudo_user {
        Some(user) if !exempt && cli.global.target.is_none() => Err(Error::Usage(format!(
            "do not run rustow with sudo: it would use root's home and root's state for the dotfiles of {user}; \
             run rustow as {user}: it asks for sudo when a system file needs it \
             (pass --target to deploy into root's home on purpose)"
        ))),
        _ => Ok(()),
    }
}

fn init_cmd(g: &Global, env: &Env, p: &mut Printer) -> Result<i32> {
    let report = crate::init::init(&global_opts(g), env, g.dry_run)?;
    for notice in &report.notices {
        p.notice(notice);
    }
    p.changes(Some(&report.root), &report.changes, g.dry_run);
    Ok(0)
}

/// `check` classifies with the `rustow file add` registrations, as `apply` does.
fn check_cmd(g: &Global, env: &Env, p: &mut Printer) -> Result<i32> {
    let mut ctx = open(g, env, p)?;
    ctx.load_state()?;
    inspect::check(&ctx, p)
}

fn file_cmd(g: &Global, env: &Env, p: &mut Printer, cmd: FileCommand) -> Result<i32> {
    match cmd {
        FileCommand::Add(a) => file_edit(g, env, p, &a.paths, files::add),
        FileCommand::Remove(a) => file_edit(g, env, p, &a.paths, files::remove),
        FileCommand::List => files::list(&mut open(g, env, p)?, p),
        FileCommand::Apply(a) => deploy(g, env, p, &a.paths, Selector::File),
        FileCommand::Diff(a) => diff_cmd(g, env, p, &a.paths, Selector::File),
    }
}

/// Runs a registration edit (`files::add` or `files::remove`) under the repository lock.
fn file_edit(
    g: &Global,
    env: &Env,
    p: &mut Printer,
    paths: &[String],
    edit: fn(&mut Context, &Printer, &[String], bool) -> Result<i32>,
) -> Result<i32> {
    let mut ctx = open(g, env, p)?;
    let _lock = lock(&ctx, p, g.dry_run)?;
    edit(&mut ctx, p, paths, g.dry_run)
}

fn template_cmd(g: &Global, env: &Env, p: &mut Printer, cmd: TemplateCommand) -> Result<i32> {
    match cmd {
        TemplateCommand::List => template_list(g, env, p),
        TemplateCommand::Apply(a) => deploy(g, env, p, &a.paths, Selector::Template),
    }
}

fn template_list(g: &Global, env: &Env, p: &mut Printer) -> Result<i32> {
    let ctx = open(g, env, p)?;
    let discovery = ctx.discover()?;
    let templates: Vec<Listed> = (discovery.sources.iter().filter(|s| s.mode.is_template()))
        .map(|s| Listed {
            source: s.path.to_string(),
            dest: Some(ctx.dest_path(s)),
            mode: s.mode.shown_label(),
        })
        .collect();
    p.list("templates", &templates);
    Ok(0)
}

fn diff_cmd(g: &Global, env: &Env, p: &mut Printer, paths: &[String], selector: Selector) -> Result<i32> {
    let prepared = prepare(open(g, env, p)?, p, paths, selector, (false, true))?;
    p.warn(prepared.ctx.skipped(&prepared.discovery, paths)?);
    p.warn(&prepared.discovery.rule_warnings);
    inspect::diff(&prepared.plan, &prepared.state, p)
}

fn status_cmd(g: &Global, env: &Env, p: &mut Printer, a: &StatusArgs) -> Result<i32> {
    let mut ctx = open(g, env, p)?;
    ctx.ignored_args = a.ignored;
    // `status` needs only hashes: each plaintext goes once it is hashed.
    let s = prepare(ctx, p, &a.paths.paths, Selector::All, (false, false))?;
    p.warn(&plan::other_targets(&s.ctx, &a.paths.paths, &s.state)?);
    inspect::status(&s.ctx, &s.plan, &s.discovery, &s.state, &a.paths.paths, a.ignored, p)
}

fn cache_clean(g: &Global, env: &Env, p: &mut Printer) -> Result<i32> {
    let ctx = open(g, env, p)?;
    let _lock = lock(&ctx, p, g.dry_run)?;
    let state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let changes = crate::cache::clean(&ctx, &state, g.dry_run)?;
    p.changes(None, &changes, g.dry_run);
    Ok(0)
}

fn state_cmd(g: &Global, env: &Env, p: &mut Printer, cmd: StateCommand) -> Result<i32> {
    let mut ctx = open(g, env, p)?;
    match cmd {
        StateCommand::Export { .. } if g.dry_run => Err(Error::Usage(
            "`rustow state export` has no --dry-run; it changes no state".into(),
        )),
        StateCommand::Export { file } => {
            crate::transfer::export(&ctx, file.as_deref(), g.policy() == Policy::Overwrite)
        }
        StateCommand::Import { file } => {
            let _lock = lock(&ctx, p, g.dry_run)?;
            crate::transfer::import(&mut ctx, p, &file, g.dry_run)
        }
        StateCommand::Adopt { old_root } => {
            let _lock = lock(&ctx, p, g.dry_run)?;
            crate::transfer::adopt(&ctx, p, &old_root, g.dry_run)
        }
    }
}

fn enc_cmd(g: &Global, env: &Env, p: &mut Printer, e: EncArgs) -> Result<i32> {
    if g.dry_run {
        return Err(Error::Usage(
            "`rustow enc` has no --dry-run; it changes only the repository".into(),
        ));
    }
    if let Some(arg) = e.extra.first() {
        return Err(Error::Usage(format!(
            "unexpected argument '{arg}'; `rustow enc` takes one PATH"
        )));
    }
    let mut ctx = open(g, env, p)?;
    let held = lock(&ctx, p, false)?;
    let mut state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let code = enc_run(&mut ctx, &mut state, e, p)?;
    save(&ctx, &state, held.as_ref())?;
    Ok(code)
}

/// Saves the state; first warns when the lock file was removed or
/// replaced during the run, so another run may have overlapped this one.
fn save(ctx: &Context, state: &State, held: Option<&Lock>) -> Result<()> {
    if let Some(lock) = held.filter(|l| l.replaced()) {
        eprintln!(
            "warning: the lock file {} was removed or replaced during this run; \
             another rustow process may have changed files at the same time",
            crate::report::shown(&lock.path.display().to_string())
        );
    }
    state.save(&ctx.state_dir, &ctx.repo_id)
}

/// Encrypts a source, or edits an encrypted one (which may prompt).
fn enc_run(ctx: &mut Context, state: &mut State, e: EncArgs, p: &Printer) -> Result<i32> {
    match e.command {
        Some(EncCommand::Edit { reencrypt, path }) => crate::enc::edit_source(ctx, state, &path, reencrypt, p),
        None => crate::enc::encrypt_source(ctx, state, e.path.as_deref().unwrap_or("."), e.backend, p),
    }
}

/// The script the shell sources: on each TAB it calls `rustow` (found on
/// PATH, so it survives upgrades) with `RUSTOW_COMPLETE` set.
fn completions(shell: clap_complete::Shell) -> Result<i32> {
    let shells = clap_complete::env::Shells::builtins();
    let name = clap::ValueEnum::to_possible_value(&shell).map(|v| v.get_name().to_owned());
    let completer = name
        .as_deref()
        .and_then(|n| shells.completer(n))
        .ok_or_else(|| Error::Usage(format!("no completion support for {shell}")))?;
    let mut script = Vec::new();
    completer
        .write_registration(crate::complete::VAR, "rustow", "rustow", "rustow", &mut script)
        .map_err(|e| Error::Failed(format!("write completion script: {e}")))?;
    crate::report::out(format_args!("{}", String::from_utf8_lossy(&script)));
    if shell == clap_complete::Shell::Zsh {
        // Autoloaded from fpath as `_rustow`, the body runs on the first TAB: complete it too.
        outln!("if [ \"$funcstack[1]\" = \"_rustow\" ]; then\n    _clap_dynamic_completer_rustow \"$@\"\nfi");
    }
    Ok(0)
}

fn global_opts(g: &Global) -> GlobalOpts {
    GlobalOpts {
        repo: g.repo.clone(),
        target: g.target.clone(),
        root_target: g.root_target.clone(),
    }
}

fn open(g: &Global, env: &Env, printer: &mut Printer) -> Result<Context> {
    let mut ctx = Context::open(&global_opts(g), env)?;
    ctx.interactive = g.interactive;
    printer.set_home(&ctx.home);
    for notice in &ctx.notices {
        printer.notice(notice);
    }
    Ok(ctx)
}

/// The repository lock, unless nothing will be written. It starts the
/// journal of temporary files, which first cleans up after an interrupted
/// run; a notice tells about each leftover. A journal that cannot start
/// (for example, a read-only state directory) only gives a warning.
fn lock(ctx: &Context, printer: &Printer, dry_run: bool) -> Result<Option<Lock>> {
    if dry_run {
        return Ok(None);
    }
    let mut lock = state::lock(&ctx.state_dir, &ctx.repo_id)?;
    match lock.start_journal() {
        Ok(notices) => notices.iter().for_each(|n| printer.shown_notice(n)),
        Err(e) => eprintln!(
            "warning: {}; the temporary files of this run are not journaled",
            crate::report::shown(&e.to_string())
        ),
    }
    Ok(Some(lock))
}

/// `lock` for a run that changes destinations. With `interruptible`, from
/// the lock on (also while rustow plans), a first signal stops the run
/// before its next change (`interrupt_flag`).
fn lock_run(ctx: &Context, printer: &Printer, dry_run: bool, interruptible: bool) -> Result<Option<Lock>> {
    let held = lock(ctx, printer, dry_run)?;
    if held.is_some() && interruptible {
        interrupt_flag()?;
    }
    Ok(held)
}

/// A plan with the context and state it was made from.
pub struct Prepared {
    pub ctx: Context,
    pub state: State,
    pub discovery: Discovery,
    pub plan: Plan,
    _lock: Option<Lock>,
}

/// State and plan for the sources a command selects in `ctx` (from
/// `open`). Takes the repository lock first when `locked`. The plan keeps
/// rendered and decrypted content only with `keep_content`.
fn prepare(
    mut ctx: Context,
    printer: &Printer,
    paths: &[String],
    selector: Selector,
    (locked, keep_content): (bool, bool),
) -> Result<Prepared> {
    let lock = lock_run(&ctx, printer, !locked, true)?;
    let mut state = ctx.load_state()?;
    let discovery = ctx.discover()?;
    let sources = selected(&ctx, &discovery, &state, paths, selector)?;
    if explicit_link(selector, paths) && ctx.config.defaults.mode == DefaultMode::Copy {
        // Only a real deploy saves the state; a dry run or a blocked run registers nothing.
        // A registered copy never gets here: it is not a link source (`nothing_deployed`).
        let links = sources.iter().map(|s| (s.path.clone(), Registration::Link));
        state.registrations.extend(links);
    }
    let plan = plan::build(&ctx, &discovery, &sources, &state, keep_content)?;
    Ok(Prepared {
        ctx,
        state,
        discovery,
        plan,
        _lock: lock,
    })
}

/// Selected sources the command deploys. A path argument that matches
/// only sources of other modes, or only files that copy mode does not
/// link, is a usage error; one that matches only skipped files selects nothing.
fn selected<'a>(
    ctx: &Context,
    discovery: &'a Discovery,
    state: &State,
    paths: &[String],
    selector: Selector,
) -> Result<Vec<&'a Source>> {
    let explicit = explicit_link(selector, paths);
    let deploys = |s: &&Source| selector.includes(&s.mode) && (explicit || may_link(ctx, state, s));
    let all = ctx.select(discovery, paths)?;
    for arg in paths {
        let one = ctx.select(discovery, std::slice::from_ref(arg))?;
        if !one.is_empty() && !one.iter().any(deploys) {
            return Err(nothing_deployed(arg, &one, selector));
        }
    }
    Ok(all.into_iter().filter(deploys).collect())
}

/// The check of `rustow link <path>` registration, also for `state
/// import`: `path` must be one file that `rustow link` would register.
pub fn check_link(ctx: &Context, discovery: &Discovery, state: &State, path: &str) -> Result<()> {
    let rel = RelPath::parse(path)?;
    let found = selected(ctx, discovery, state, &[path.to_owned()], Selector::Link)?;
    match found.iter().any(|s| s.path == rel) {
        true => Ok(()),
        false => Err(Error::Usage(format!(
            "'{rel}' is a directory; a link registration names a file"
        ))),
    }
}

/// The usage error for a path argument whose sources this command does not deploy.
fn nothing_deployed(arg: &str, found: &[&Source], selector: Selector) -> Error {
    if found.iter().any(|s| selector.includes(&s.mode)) {
        return Error::Usage(format!(
            "'{arg}' is not linked; in copy mode, `rustow link {arg}` links a plain file and `rustow file add {arg}` registers a copy"
        ));
    }
    // A system file never links, also without its registration.
    let copy = |s: &&&Source| s.scope == Scope::Home && matches!(s.mode, Mode::FileCopy(_) | Mode::DecryptCopy(..));
    if let Some(s) = found.iter().find(copy).filter(|_| selector == Selector::Link) {
        return Error::Usage(format!(
            "'{}' is registered as a copy; run `rustow file remove {}` first to link it",
            s.path, s.path
        ));
    }
    let modes: Vec<&str> = found.iter().map(|s| s.mode.label()).collect();
    Error::Usage(format!(
        "'{arg}' has no sources this command deploys (found: {}); use `rustow apply`",
        modes.join(", ")
    ))
}

/// `rustow link <path>`: the command names the files to link.
fn explicit_link(selector: Selector, paths: &[String]) -> bool {
    selector == Selector::Link && !paths.is_empty()
}

/// Copy mode makes no new symlink by itself: a link source deploys only
/// when `rustow link <path>` registered it in the state.
fn may_link(ctx: &Context, state: &State, source: &Source) -> bool {
    ctx.config.defaults.mode == DefaultMode::Symlink
        || !matches!(source.mode, Mode::Link | Mode::DecryptLink(_))
        || state.is_link(&source.path)
}

/// `unlink` drops the link registrations at or below each source it removes.
fn forget_links(state: &mut State, selector: Selector, removals: &[plan::Removal]) {
    if selector == Selector::Link {
        let removed = |p: &RelPath| removals.iter().any(|r| p.starts_with(&r.source));
        state
            .registrations
            .retain(|p, kind| *kind != Registration::Link || !removed(p));
    }
}

fn deploy(g: &Global, env: &Env, printer: &mut Printer, paths: &[String], selector: Selector) -> Result<i32> {
    let Prepared {
        ctx,
        mut state,
        discovery,
        plan,
        _lock: held,
    } = prepare(open(g, env, printer)?, printer, paths, selector, (!g.dry_run, true))?;
    printer.warn(ctx.skipped(&discovery, paths)?);
    printer.warn(&discovery.rule_warnings);
    let orphaned = inspect::orphan_reports(&ctx, &discovery, &state, paths, selector)?;
    let policy = g.policy();
    if g.dry_run || (policy == Policy::Abort && execute::blocked(&plan, policy)) {
        return Ok(unrun(&plan, policy, orphaned, g.dry_run, printer));
    }
    let stop = interrupt_flag()?;
    let backup_dir =
        ctx.state_dir
            .join("backups")
            .join(&ctx.repo_id)
            .join(format!("{}-{}", fsops::timestamp(), std::process::id()));
    let run = Run {
        policy,
        backup_dir: &backup_dir,
        cache_dir: &ctx.cache_dir,
        stop: &stop,
    };
    let mut items = execute::execute(plan, &run, &mut state);
    items.extend(orphaned);
    finish(&ctx, &mut state, held.as_ref(), printer, &items)
}

/// Prints what each item of `plan` would do under `policy`, for a dry run
/// or a run that the abort policy stopped, and gives the exit code.
fn unrun(plan: &Plan, policy: Policy, orphaned: Vec<ItemReport>, dry_run: bool, printer: &Printer) -> i32 {
    let predicted = plan.items.iter().map(|i| {
        let (action, detail) = execute::predict(i, policy);
        ItemReport::of(i, action, detail)
    });
    let items: Vec<ItemReport> = predicted.chain(orphaned).collect();
    printer.items(&items, if dry_run { RunKind::DryRun } else { RunKind::Blocked });
    execute::exit_code(&items)
}

/// Reports the items first, so a state write failure cannot hide what changed on disk.
fn finish(
    ctx: &Context,
    state: &mut State,
    held: Option<&Lock>,
    printer: &Printer,
    items: &[ItemReport],
) -> Result<i32> {
    state.forget_missing_dirs();
    let saved = save(ctx, state, held);
    printer.items(items, RunKind::Applied);
    saved.map(|()| execute::exit_code(items))
}

/// Removes managed destinations. With `ask`, a real run that removes
/// anything shows the list and needs a "yes" first.
fn undeploy(
    g: &Global,
    env: &Env,
    printer: &mut Printer,
    paths: &[String],
    selector: Selector,
    ask: bool,
) -> Result<i32> {
    let abort = removal_policy(g, selector)? == Policy::Abort;
    let ctx = open(g, env, printer)?;
    let held = lock_run(&ctx, printer, g.dry_run, !ask)?;
    let mut state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let (removals, elsewhere) = plan::plan_removal(&ctx, paths, selector, &state)?;
    printer.warn(&elsewhere);
    let predicted = execute::predict_removal(&removals);
    if let Some(code) = removes_nothing(g.dry_run, (abort, ask), printer, &predicted)? {
        return Ok(code);
    }
    let stop = interrupt_flag()?;
    forget_links(&mut state, selector, &removals);
    let items = execute::remove(removals, &stop, &mut state, &ctx.cache_dir);
    finish(&ctx, &mut state, held.as_ref(), printer, &items)
}

/// The policy of `undeploy` and `unlink`: they never remove a changed
/// destination, so only `skip` and `abort` apply.
fn removal_policy(g: &Global, selector: Selector) -> Result<Policy> {
    let flag = match (g.policy(), g.backup, g.force) {
        (p @ (Policy::Skip | Policy::Abort), ..) => return Ok(p),
        (_, true, _) => "--backup",
        (_, _, true) => "--force",
        (Policy::Backup, ..) => "--on-conflict backup",
        _ => "--on-conflict overwrite",
    };
    let cmd = if selector == Selector::Link {
        "unlink"
    } else {
        "undeploy"
    };
    Err(Error::Usage(format!(
        "`rustow {cmd}` does not take {flag}: it never removes a destination that changed; \
         move or remove such a file yourself"
    )))
}

/// The exit code of an `undeploy` or `unlink` that removes nothing: a dry
/// run, a conflict or an error under `abort`, or no "yes" to the question.
fn removes_nothing(
    dry_run: bool,
    (abort, ask): (bool, bool),
    printer: &Printer,
    predicted: &[ItemReport],
) -> Result<Option<i32>> {
    let blocked = abort && execute::exit_code(predicted) != 0;
    if dry_run || blocked {
        printer.items(predicted, if dry_run { RunKind::DryRun } else { RunKind::Blocked });
        return Ok(Some(execute::exit_code(predicted)));
    }
    if !confirm_removal(ask, printer, predicted)? {
        eprintln!("cancelled, nothing removed");
        return Ok(Some(0));
    }
    Ok(None)
}

/// With `ask`, shows on stderr what would go and asks on the terminal.
/// True when nothing would be removed. Without a terminal, only `--yes` confirms.
fn confirm_removal(ask: bool, printer: &Printer, items: &[ItemReport]) -> Result<bool> {
    use std::io::{BufRead, IsTerminal};
    let n = items.iter().filter(|i| i.action == Action::Remove).count();
    if !ask || n == 0 {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Usage(
            "`rustow undeploy` without paths removes every managed destination; add --yes to confirm".into(),
        ));
    }
    for i in items {
        eprintln!(
            "{:>8} {}  <- {} ({})",
            i.action.as_str(),
            printer.path(&i.dest),
            crate::report::shown(i.source.as_str()),
            i.mode
        );
    }
    eprint!("Remove {}? [y/N] ", crate::report::plural(n, "destination"));
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| Error::Failed(format!("read the answer: {e}")))?;
    Ok(is_yes(&answer))
}

fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The first SIGINT/SIGTERM/SIGHUP asks the executor (or `enc edit`) to
/// stop; a second one terminates at once. The first call installs the
/// handlers; each later call returns the same flag.
pub(crate) fn interrupt_flag() -> Result<Arc<AtomicBool>> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    if let Some(stop) = STOP.get() {
        return Ok(Arc::clone(stop));
    }
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [SIGINT, SIGTERM, SIGHUP] {
        signal_hook::flag::register_conditional_shutdown(sig, 130, Arc::clone(&stop))
            .and_then(|_| signal_hook::flag::register(sig, Arc::clone(&stop)))
            .map_err(|e| Error::Failed(format!("install signal handler: {e}")))?;
    }
    Ok(Arc::clone(STOP.get_or_init(|| stop)))
}

#[cfg(test)]
mod tests {
    use super::{is_yes, refuse_sudo};
    use crate::cli::Cli;
    use clap::Parser;

    #[test]
    fn sudo_without_a_target_is_refused() {
        let cli = |args: &[&str]| Cli::parse_from([&["rustow"], args].concat());
        let e = refuse_sudo(&cli(&["status"]), Some("alice")).unwrap_err().to_string();
        assert!(
            e.contains("do not run rustow with sudo") && e.contains("as alice"),
            "{e}"
        );
        assert!(refuse_sudo(&cli(&[]), Some("alice")).is_err(), "the default apply too");
        assert!(
            refuse_sudo(&cli(&["--target", "/root", "apply"]), Some("alice")).is_ok(),
            "an explicit --target is a real root setup"
        );
        assert!(refuse_sudo(&cli(&["completions", "bash"]), Some("alice")).is_ok());
        assert!(refuse_sudo(&cli(&["status"]), None).is_ok());
    }

    #[test]
    fn only_y_or_yes_confirms() {
        for a in ["y\n", "Y\n", "yes\n", " YES \r\n"] {
            assert!(is_yes(a), "{a:?}");
        }
        for a in ["", "\n", "n\n", "no\n", "yess\n", "y es\n"] {
            assert!(!is_yes(a), "{a:?}");
        }
    }
}
