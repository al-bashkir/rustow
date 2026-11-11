use clap::Parser;

fn main() {
    let cli = rustow::cli::Cli::parse();
    let code = match rustow::app::run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rustow: error: {err}");
            err.exit_code()
        }
    };
    std::process::exit(code);
}
