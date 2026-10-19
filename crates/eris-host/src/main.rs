//! `eris` - the Kallisti 1.0 daemon binary.
//!
//! P4 command surface: `probe` and nothing else. There is no listener, no
//! database, no session log, and no turn machine yet - those are P5 and later.
//! With no subcommand the binary prints one version line and exits, which is
//! what the Wave 0 stub did and what a package sanity check expects.
//!
//! The probe is the phase's proof: a configured OpenAI-compatible endpoint, a
//! key resolved from `env:NAME`, one streamed prompt, and the usage line the
//! provider actually reported.

mod config;
mod probe;

use std::io;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Process exit code for a failed probe.
const EXIT_FAILURE: u8 = 1;

#[derive(Debug, Parser)]
#[command(
    name = "eris",
    version,
    about = "Kallisti 1.0 daemon (Eris)",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send one prompt through the configured provider, print the reply and usage.
    Probe {
        /// The prompt to send.
        prompt: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        None => {
            println!(
                "eris {} (core-domain {})",
                env!("CARGO_PKG_VERSION"),
                core_domain::VERSION
            );
            ExitCode::SUCCESS
        }
        Some(Command::Probe { prompt }) => run_probe(&prompt),
    }
}

fn run_probe(prompt: &str) -> ExitCode {
    let config = match config::KallistiConfig::default_path()
        .and_then(|path| config::KallistiConfig::load(&path))
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!("eris probe: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("eris probe: cannot start the runtime: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut diag = stderr.lock();

    let result = runtime.block_on(probe::run(&config, prompt, &mut out, &mut diag));

    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            // The error variants carry provider body text or a key *reference*,
            // never a key value.
            eprintln!("eris probe: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}
