//! Command dispatch: builds the context and runs one command.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::cli::{Cli, Command, FileCommand, Global, Paths, TemplateCommand};
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
        Command::Init => {
            let report = crate::init::init(&global_opts(g), &env, g.dry_run)?;
            for notice in &report.notices {
                p.notice(notice);
            }
            p.changes(Some(&report.root), &report.changes, g.dry_run);
            Ok(0)
        }
        Command::File(FileCommand::Add(a)) => {
            let ctx = open(g, &env, p)?;
            let _lock = lock(&ctx, g.dry_run)?;
            files::add(&ctx, p, &a.paths, g.dry_run)
        }
        Command::File(FileCommand::Remove(a)) => {
            let ctx = open(g, &env, p)?;
            let _lock = lock(&ctx, g.dry_run)?;
            files::remove(&ctx, p, &a.paths, g.dry_run)
        }
        Command::File(FileCommand::List) => files::list(&open(g, &env, p)?, p),
        Command::File(FileCommand::Apply(a)) => deploy(g, &env, p, &a.paths, Selector::File),
        Command::File(FileCommand::Diff(a)) => {
            let prepared = prepare(g, &env, p, &a.paths, Selector::File, false)?;
            inspect::diff(&prepared.plan, p)
        }
        Command::Template(TemplateCommand::List) => {
            let ctx = open(g, &env, p)?;
            let discovery = ctx.discover()?;
            let templates: Vec<&Source> = discovery.sources.iter().filter(|s| s.mode.is_template()).collect();
            p.sources(&ctx, &templates, "templates");
            Ok(0)
        }
        Command::Template(TemplateCommand::Apply(a)) => deploy(g, &env, p, &a.paths, Selector::Template),
        Command::Diff(a) => {
            let prepared = prepare(g, &env, p, &a.paths, Selector::All, false)?;
            inspect::diff(&prepared.plan, p)
        }
    }
}

fn global_opts(g: &Global) -> GlobalOpts {
    GlobalOpts {
        repo: g.repo.clone(),
        target: g.target.clone(),
        root_target: g.root_target.clone(),
    }
}

fn open(g: &Global, env: &Env, printer: &mut Printer) -> Result<Context> {
    let ctx = Context::open(&global_opts(g), env)?;
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
        plan,
        _lock: lock,
    })
}

/// Selected sources the command deploys. A path argument that matches
/// only sources of other modes is a usage error.
fn selected<'a>(
    ctx: &Context,
    discovery: &'a Discovery,
    paths: &[String],
    selector: Selector,
) -> Result<Vec<&'a Source>> {
    let all = ctx.select(discovery, paths)?;
    for arg in paths {
        let one = ctx.select(discovery, std::slice::from_ref(arg))?;
        if !one.iter().any(|s| selector.includes(&s.mode)) {
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
        plan,
        _lock,
    } = prepare(g, env, printer, paths, selector, !g.dry_run)?;
    let policy = g.policy();
    if g.dry_run || (policy == Policy::Abort && execute::blocked(&plan, policy)) {
        let items: Vec<ItemReport> = plan
            .items
            .iter()
            .map(|i| {
                let (action, detail) = execute::predict(i, policy);
                let mode = i.source.mode.label().to_owned();
                ItemReport {
                    source: i.source.path.clone(),
                    dest: i.dest.clone(),
                    mode,
                    action,
                    detail,
                }
            })
            .collect();
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
    let items = execute::execute(plan, &run, &mut state);
    state.save(&ctx.state_dir, &ctx.repo_id)?;
    printer.items(&items, RunKind::Applied);
    Ok(execute::exit_code(&items))
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
    state.save(&ctx.state_dir, &ctx.repo_id)?;
    printer.items(&items, RunKind::Applied);
    Ok(execute::exit_code(&items))
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
