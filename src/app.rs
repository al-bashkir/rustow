//! Command dispatch: builds the context and runs one command.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::cli::{
    CacheCommand, Cli, Command, EncArgs, EncCommand, FileCommand, Global, Paths, StatusArgs, TemplateCommand,
};
use crate::error::{Error, Result};
use crate::execute::{self, ItemReport, Policy, Run};
use crate::plan::{self, Plan};
use crate::repo::{Context, Env, GlobalOpts};
use crate::report::{Printer, RunKind};
use crate::source::{Discovery, Selector, Source};
use crate::state::{self, Lock, State};
use crate::{files, fsops, inspect};

/// Runs the parsed command and returns the process exit code.
pub fn run(cli: Cli) -> Result<i32> {
    let env = Env::from_process()?;
    let g = &cli.global;
    let mut printer = Printer::new(g.json, g.quiet, g.verbose, g.no_color);
    let p = &mut printer;
    match cli.command.unwrap_or(Command::Apply(Paths::default())) {
        Command::Apply(a) => deploy(g, &env, p, &a.paths, Selector::All),
        Command::Link(a) => deploy(g, &env, p, &a.paths, Selector::Link),
        Command::Unlink(a) => undeploy(g, &env, p, &a.paths, Selector::Link),
        Command::Init => init_cmd(g, &env, p),
        Command::File(c) => file_cmd(g, &env, p, c),
        Command::Template(c) => template_cmd(g, &env, p, c),
        Command::Diff(a) => diff_cmd(g, &env, p, &a.paths, Selector::All),
        Command::Status(a) => status_cmd(g, &env, p, &a),
        Command::Check => inspect::check(&open(g, &env, p)?, p),
        Command::Doctor => crate::doctor::doctor(&open(g, &env, p)?, p),
        Command::Undeploy(a) => undeploy(g, &env, p, &a.paths, Selector::All),
        Command::Cache(CacheCommand::Clean) => cache_clean(g, &env, p),
        Command::Enc(e) => enc_cmd(g, &env, p, e),
        Command::Completions { shell } => completions(shell),
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

fn file_cmd(g: &Global, env: &Env, p: &mut Printer, cmd: FileCommand) -> Result<i32> {
    match cmd {
        FileCommand::Add(a) => file_edit(g, env, p, &a.paths, files::add),
        FileCommand::Remove(a) => file_edit(g, env, p, &a.paths, files::remove),
        FileCommand::List => files::list(&open(g, env, p)?, p),
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
    edit: fn(&Context, &Printer, &[String], bool) -> Result<i32>,
) -> Result<i32> {
    let ctx = open(g, env, p)?;
    let _lock = lock(&ctx, g.dry_run)?;
    edit(&ctx, p, paths, g.dry_run)
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
    let templates: Vec<&Source> = discovery.sources.iter().filter(|s| s.mode.is_template()).collect();
    p.sources(&ctx, &templates, "templates");
    Ok(0)
}

fn diff_cmd(g: &Global, env: &Env, p: &mut Printer, paths: &[String], selector: Selector) -> Result<i32> {
    let prepared = prepare(g, env, p, paths, selector, false)?;
    p.warn(prepared.ctx.skipped(&prepared.discovery, paths)?);
    inspect::diff(&prepared.plan, &prepared.state, p)
}

fn status_cmd(g: &Global, env: &Env, p: &mut Printer, a: &StatusArgs) -> Result<i32> {
    let s = prepare(g, env, p, &a.paths.paths, Selector::All, false)?;
    if let Some(skip) = s.discovery.skipped.first() {
        // shortcut: status refuses a skipped source until it shows SKIPPED rows (next commit).
        return Err(Error::Config(format!("source '{}': {}", skip.path, skip.reason)));
    }
    inspect::status(&s.ctx, &s.plan, &s.discovery, &s.state, &a.paths.paths, a.ignored, p)
}

fn cache_clean(g: &Global, env: &Env, p: &mut Printer) -> Result<i32> {
    let ctx = open(g, env, p)?;
    let _lock = lock(&ctx, g.dry_run)?;
    let state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let changes = crate::cache::clean(&ctx, &state, g.dry_run)?;
    p.changes(None, &changes, g.dry_run);
    Ok(0)
}

fn enc_cmd(g: &Global, env: &Env, p: &mut Printer, e: EncArgs) -> Result<i32> {
    if g.dry_run {
        return Err(Error::Usage(
            "`rustow enc` has no --dry-run; it changes only the repository".into(),
        ));
    }
    let mut ctx = open(g, env, p)?;
    let _lock = lock(&ctx, false)?;
    let mut state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let code = enc_run(&mut ctx, &mut state, e, p)?;
    state.save(&ctx.state_dir, &ctx.repo_id)?;
    Ok(code)
}

/// Encrypts a source, or edits an encrypted one (which may prompt).
fn enc_run(ctx: &mut Context, state: &mut State, e: EncArgs, p: &Printer) -> Result<i32> {
    match e.command {
        Some(EncCommand::Edit { reencrypt, path }) => crate::enc::edit_source(ctx, state, &path, reencrypt, p),
        None => crate::enc::encrypt_source(ctx, state, e.path.as_deref().unwrap_or("."), e.backend, p),
    }
}

fn completions(shell: clap_complete::Shell) -> Result<i32> {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "rustow", &mut std::io::stdout());
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

/// The repository lock, unless nothing will be written.
fn lock(ctx: &Context, dry_run: bool) -> Result<Option<Lock>> {
    if dry_run {
        Ok(None)
    } else {
        state::lock(&ctx.state_dir, &ctx.repo_id).map(Some)
    }
}

/// A plan with the context and state it was made from.
pub struct Prepared {
    pub ctx: Context,
    pub state: State,
    pub discovery: Discovery,
    pub plan: Plan,
    _lock: Option<Lock>,
}

/// Context, state and plan for the sources a command selects. Takes the
/// repository lock first when `locked`.
fn prepare(
    g: &Global,
    env: &Env,
    printer: &mut Printer,
    paths: &[String],
    selector: Selector,
    locked: bool,
) -> Result<Prepared> {
    let ctx = open(g, env, printer)?;
    let lock = lock(&ctx, !locked)?;
    let state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let discovery = ctx.discover()?;
    let sources = selected(&ctx, &discovery, paths, selector)?;
    let plan = plan::build(&ctx, &discovery, &sources, &state)?;
    Ok(Prepared {
        ctx,
        state,
        discovery,
        plan,
        _lock: lock,
    })
}

/// Selected sources the command deploys. A path argument that matches
/// only sources of other modes is a usage error; one that matches only
/// skipped files selects nothing.
fn selected<'a>(
    ctx: &Context,
    discovery: &'a Discovery,
    paths: &[String],
    selector: Selector,
) -> Result<Vec<&'a Source>> {
    let all = ctx.select(discovery, paths)?;
    for arg in paths {
        let one = ctx.select(discovery, std::slice::from_ref(arg))?;
        if !one.is_empty() && !one.iter().any(|s| selector.includes(&s.mode)) {
            let modes: Vec<&str> = one.iter().map(|s| s.mode.label()).collect();
            return Err(Error::Usage(format!(
                "'{arg}' has no sources this command deploys (found: {}); use `rustow apply`",
                modes.join(", ")
            )));
        }
    }
    Ok(all.into_iter().filter(|s| selector.includes(&s.mode)).collect())
}

fn deploy(g: &Global, env: &Env, printer: &mut Printer, paths: &[String], selector: Selector) -> Result<i32> {
    let Prepared {
        ctx,
        mut state,
        discovery,
        plan,
        _lock,
    } = prepare(g, env, printer, paths, selector, !g.dry_run)?;
    printer.warn(ctx.skipped(&discovery, paths)?);
    let orphaned = inspect::orphan_reports(&ctx, &discovery, &state, paths, selector)?;
    let policy = g.policy();
    if g.dry_run || (policy == Policy::Abort && execute::blocked(&plan, policy)) {
        let mut items: Vec<ItemReport> = plan
            .items
            .iter()
            .map(|i| {
                let (action, detail) = execute::predict(i, policy);
                let mode = i.source.mode.shown_label();
                ItemReport {
                    source: i.source.path.clone(),
                    dest: i.dest.clone(),
                    mode,
                    action,
                    detail,
                }
            })
            .collect();
        items.extend(orphaned);
        printer.items(&items, if g.dry_run { RunKind::DryRun } else { RunKind::Blocked });
        return Ok(execute::exit_code(&items));
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
    finish(&ctx, &state, printer, &items)
}

/// Reports the items first, so a state write failure cannot hide what changed on disk.
fn finish(ctx: &Context, state: &State, printer: &Printer, items: &[ItemReport]) -> Result<i32> {
    let saved = state.save(&ctx.state_dir, &ctx.repo_id);
    printer.items(items, RunKind::Applied);
    saved.map(|()| execute::exit_code(items))
}

fn undeploy(g: &Global, env: &Env, printer: &mut Printer, paths: &[String], selector: Selector) -> Result<i32> {
    let ctx = open(g, env, printer)?;
    let _lock = lock(&ctx, g.dry_run)?;
    let mut state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let removals = plan::plan_removal(&ctx, paths, selector, &state)?;
    if g.dry_run {
        let items = execute::predict_removal(&removals);
        printer.items(&items, RunKind::DryRun);
        return Ok(execute::exit_code(&items));
    }
    let stop = interrupt_flag()?;
    let items = execute::remove(removals, &stop, &mut state);
    finish(&ctx, &state, printer, &items)
}

/// The first SIGINT/SIGTERM/SIGHUP asks the executor to stop between
/// items; a second one terminates at once.
fn interrupt_flag() -> Result<Arc<AtomicBool>> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [SIGINT, SIGTERM, SIGHUP] {
        signal_hook::flag::register_conditional_shutdown(sig, 130, Arc::clone(&stop))
            .and_then(|_| signal_hook::flag::register(sig, Arc::clone(&stop)))
            .map_err(|e| Error::Failed(format!("install signal handler: {e}")))?;
    }
    Ok(stop)
}
