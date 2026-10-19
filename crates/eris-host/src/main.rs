//! `eris` - the Kallisti 1.0 daemon binary.
//!
//! P5 command surface. `serve` is the default: with no subcommand, the daemon
//! starts. `probe` is the P4 provider proof, `doctor` reports what it verified,
//! `version` prints one line, and `setup`, `device` and `eval` are placeholders
//! that exit non-zero on purpose - a placeholder that exits 0 looks like a
//! working feature to every script that calls it.
//!
//! Nothing here installs a service, binds the device port, or touches another
//! process on the host.

use std::io;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use eris_host::config::{LoadedConfig, LogLevel, Paths};
use eris_host::db::Db;
use eris_host::{admin, doctor, probe};

/// Exit code for a failure.
const EXIT_FAILURE: u8 = 1;
/// Exit code for a subcommand that is not implemented in this build.
const EXIT_NOT_IMPLEMENTED: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "eris",
    version,
    about = "Kallisti 1.0 daemon (Eris)",
    long_about = None
)]
struct Cli {
    /// Emit one machine-readable JSON object (doctor, setup, device, eval).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the admin listener on 127.0.0.1:7340. The default subcommand.
    Serve,
    /// Send one prompt through the configured provider, print the reply and usage.
    Probe {
        /// The prompt to send.
        prompt: String,
    },
    /// Placeholder. Onboarding is P7 and no handler is registered yet.
    Setup,
    /// Placeholder. The device listener is P8 and 7341 stays unbound.
    Device,
    /// Report what this process verified: config, database, provider reachability.
    Doctor,
    /// Placeholder. The evaluation harness is not part of P5.
    Eval,
    /// Print the version line.
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        // `serve` is the default: no subcommand starts the daemon.
        None | Some(Command::Serve) => run_serve(),
        Some(Command::Probe { prompt }) => run_probe(&prompt),
        Some(Command::Setup) => not_implemented(
            cli.json,
            "setup",
            "onboarding is P7: no setup routes or handlers exist in this build",
        ),
        Some(Command::Device) => not_implemented(
            cli.json,
            "device",
            "the device listener is P8: 127.0.0.1:7341 is not bound by this build",
        ),
        Some(Command::Eval) => not_implemented(
            cli.json,
            "eval",
            "the evaluation harness is not implemented: P5 ships serve, probe and doctor",
        ),
        Some(Command::Doctor) => run_doctor(cli.json),
        Some(Command::Version) => {
            println!("eris {}", admin::VERSION);
            ExitCode::SUCCESS
        }
    }
}

/// Print a deliberate placeholder and exit non-zero.
fn not_implemented(json: bool, name: &str, message: &str) -> ExitCode {
    if json {
        println!(r#"{{"status":"not_implemented","command":"{name}","message":"{message}"}}"#);
    } else {
        eprintln!("eris {name}: {message}");
    }
    ExitCode::from(EXIT_NOT_IMPLEMENTED)
}

fn run_serve() -> ExitCode {
    let loaded = match load() {
        Ok(loaded) => loaded,
        Err(message) => {
            eprintln!("eris serve: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    init_logging(loaded.toml().log_level());

    // The database is opened before the listener: a daemon that cannot reach its
    // state must not answer health checks.
    let db = match Db::open(&loaded.paths().db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris serve: cannot open {}: {error}",
                loaded.paths().db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let setup_state = match db.setup_state() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("eris serve: cannot read the setup state: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    tracing::info!(
        db = %loaded.paths().db_file.display(),
        workspace = %loaded.toml().workspace_path().display(),
        setup_state = setup_state.as_str(),
        "eris starting"
    );

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("eris serve: cannot start the runtime: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    match runtime.block_on(admin::serve(db)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("eris serve: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn run_doctor(json: bool) -> ExitCode {
    let report = doctor::run_default();
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_text());
    }
    if report.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_FAILURE)
    }
}

fn run_probe(prompt: &str) -> ExitCode {
    let config = match eris_host::config::KallistiConfig::default_path()
        .and_then(|path| eris_host::config::KallistiConfig::load(&path))
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

/// Resolve the paths and load both config halves, flattening the error to one
/// line for the operator.
fn load() -> Result<LoadedConfig, String> {
    let paths = Paths::from_home().map_err(|error| error.to_string())?;
    LoadedConfig::load(paths).map_err(|error| error.to_string())
}

/// Install a stderr logger at the configured level.
fn init_logging(level: LogLevel) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(level.as_tracing())
        .with_target(false)
        .with_writer(io::stderr)
        .try_init();
}
