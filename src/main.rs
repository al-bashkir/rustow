fn main() {
    // A TAB in the shell runs rustow with RUSTOW_COMPLETE set: answer and exit.
    clap_complete::CompleteEnv::with_factory(rustow::cli::command)
        .var(rustow::complete::VAR)
        .shells(rustow::complete::SHELLS)
        .complete();
    let cli = rustow::cli::try_parse_from(std::env::args_os()).unwrap_or_else(|e| e.exit());
    let code = match rustow::app::catch_file_size_signal().and_then(|()| rustow::app::run(cli)) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rustow: error: {}", rustow::report::shown(&err.to_string()));
            err.exit_code()
        }
    };
    std::process::exit(code);
}
