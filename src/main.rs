#![forbid(unsafe_code)]

use std::env;
use std::io::{self, Write};
use std::process;

use rotalog::cli::{self, Invocation};
use rotalog::{Result, runner};

fn main() {
    if let Err(error) = run() {
        eprintln!("rotalog: {error}");
        process::exit(error.exit_code());
    }
}

fn run() -> Result<()> {
    match cli::parse(env::args_os().skip(1))? {
        Invocation::Help => {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(cli::HELP.as_bytes());
            let _ = stdout.flush();
            Ok(())
        }
        Invocation::Version => {
            let mut stdout = io::stdout().lock();
            let _ = writeln!(stdout, "rotalog {}", env!("CARGO_PKG_VERSION"));
            let _ = stdout.flush();
            Ok(())
        }
        Invocation::Run(config) => {
            if config.verbose {
                config.dump();
            }
            runner::run(config)
        }
    }
}
