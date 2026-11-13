use clap::{CommandFactory, Parser};

fn main() {
    // A TAB in the shell runs rustow with RUSTOW_COMPLETE set: answer and exit.
    clap_complete::CompleteEnv::with_factory(rustow::cli::Cli::command)
        .var(rustow::complete::VAR)
        .complete();
    let cli = rustow::cli::Cli::parse();
    let code = match rustow::app::run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rustow: error: {}", rustow::report::shown(&err.to_string()));
            err.exit_code()
        }
    };
    std::process::exit(code);
}
