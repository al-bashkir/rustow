//! Command dispatch: builds the context and runs one command.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::cli::{Cli, Command, Global};
use crate::error::{Error, Result};
use crate::execute::{self, Policy, Run};
use crate::fsops;
use crate::plan;
use crate::repo::{Context, Env, GlobalOpts};
use crate::report::{Printer, RunKind};
use crate::source::{Selector, Source};
use crate::state::{self, State};

/// Runs the parsed command and returns the process exit code.
pub fn run(cli: Cli) -> Result<i32> {
    let env = Env::from_process()?;
    let mut printer = Printer::new(
        cli.global.json,
        cli.global.quiet,
        cli.global.verbose,
        cli.global.no_color,
    );
    let g = &cli.global;
    match cli
        .command
        .unwrap_or(Command::Apply(crate::cli::Paths { paths: Vec::new() }))
    {
        Command::Apply(p) => deploy(g, &env, &mut printer, &p.paths, Selector::All),
        Command::Link(p) => deploy(g, &env, &mut printer, &p.paths, Selector::Link),
        Command::Unlink(p) => undeploy(g, &env, &mut printer, &p.paths, Selector::Link),
    }
}

fn open(g: &Global, env: &Env, printer: &mut Printer) -> Result<Context> {
    let opts = GlobalOpts {
        repo: g.repo.clone(),
        target: g.target.clone(),
        root_target: g.root_target.clone(),
    };
    let ctx = Context::open(&opts, env)?;
    printer.set_home(&ctx.home);
    for notice in &ctx.notices {
        printer.notice(notice);
    }
    Ok(ctx)
}

/// Selected sources the command deploys. A path argument that matches
/// only sources of other modes is a usage error.
fn selected<'a>(
    ctx: &Context,
    discovery: &'a crate::source::Discovery,
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
    Ok(all
        .into_iter()
        .filter(|s| selector.includes(&s.mode))
        .collect())
}

fn deploy(
    g: &Global,
    env: &Env,
    printer: &mut Printer,
    paths: &[String],
    selector: Selector,
) -> Result<i32> {
    let ctx = open(g, env, printer)?;
    let _lock = if g.dry_run {
        None
    } else {
        Some(state::lock(&ctx.state_dir, &ctx.repo_id)?)
    };
    let mut state = State::load(&ctx.state_dir, &ctx.repo_id, &ctx.root)?;
    let discovery = ctx.discover()?;
    let sources = selected(&ctx, &discovery, paths, selector)?;
    let plan = plan::build(&ctx, &discovery, &sources, &state)?;
    let policy = g.policy();
    if g.dry_run || (policy == Policy::Abort && execute::blocked(&plan, policy)) {
        let items: Vec<_> = plan
            .items
            .iter()
            .map(|i| {
                let (action, detail) = execute::predict(i, policy);
                execute::ItemReport {
                    source: i.source.path.clone(),
                    dest: i.dest.clone(),
                    mode: i.source.mode.label().to_owned(),
                    action,
                    detail,
                }
            })
            .collect();
        printer.items(
            &items,
            if g.dry_run {
                RunKind::DryRun
            } else {
                RunKind::Blocked
            },
        );
        return Ok(execute::exit_code(&items));
    }
    let stop = interrupt_flag()?;
    let backup_dir = ctx
        .state_dir
        .join("backups")
        .join(&ctx.repo_id)
        .join(format!("{}-{}", fsops::timestamp(), std::process::id()));
    let run = Run {
        policy,
        backup_dir: &backup_dir,
        stop: &stop,
    };
    let items = execute::execute(plan, &run, &mut state);
    state.save(&ctx.state_dir, &ctx.repo_id)?;
    printer.items(&items, RunKind::Applied);
    Ok(execute::exit_code(&items))
}

fn undeploy(
    g: &Global,
    env: &Env,
    printer: &mut Printer,
    paths: &[String],
    selector: Selector,
) -> Result<i32> {
    let ctx = open(g, env, printer)?;
    let _lock = if g.dry_run {
        None
    } else {
        Some(state::lock(&ctx.state_dir, &ctx.repo_id)?)
    };
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
