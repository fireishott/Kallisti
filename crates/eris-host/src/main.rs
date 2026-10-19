//! `eris` - the Kallisti 1.0 daemon binary.
//!
//! Command surface as of P7. `serve` is the default: with no subcommand, the
//! daemon starts. `probe` is the P4 provider proof, `doctor` reports what it
//! verified, `setup` drives the first-run recovery path, `device` and `eval` are
//! placeholders that exit non-zero on purpose (a placeholder that exits 0 looks
//! like a working feature to every script that calls it), and `version` prints
//! one line.
//!
//! `serve` is the only command that binds a port, and the port is a literal in
//! `admin.rs`. Nothing here installs a service, binds the device port, or touches
//! another process on the host.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use eris_host::admin::AppState;
use eris_host::config::{LoadedConfig, LogLevel, Paths};
use eris_host::db::Db;
use eris_host::onboarding::{SetupEngine, first_run_block, pending_block};
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

    /// Use this root instead of `$HOME` for config and state. The layout is
    /// `config/` and `state/` under the root, so a scratch root never reads or
    /// writes the installed database. Applies to every subcommand.
    #[arg(long, global = true, value_name = "PATH")]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the admin listener on 127.0.0.1:7340. The default subcommand.
    Serve {
        /// Allow `full` sandbox mode to be stored. Without this flag, `full` is
        /// refused by onboarding and by the dashboard, and only a restart with
        /// the flag can change that.
        #[arg(long)]
        allow_full: bool,
    },
    /// Send one prompt through the configured provider, print the reply and usage.
    Probe {
        /// The prompt to send.
        prompt: String,
    },
    /// Report onboarding progress, or reopen the wizard with `--reset`.
    Setup {
        /// Reopen the wizard. Requires the existing admin passphrase on stdin.
        #[arg(long)]
        reset: bool,
    },
    /// Placeholder. The device listener is P8 and 7341 stays unbound.
    Device,
    /// Report what this process verified: config, database, provider reachability.
    Doctor,
    /// Placeholder. The evaluation harness is not part of this build.
    Eval,
    /// Print the version line.
    Version,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        // `serve` is the default: no subcommand starts the daemon.
        None => run_serve(cli.state_dir, false),
        Some(Command::Serve { allow_full }) => run_serve(cli.state_dir, allow_full),
        Some(Command::Probe { prompt }) => run_probe(&prompt),
        Some(Command::Setup { reset }) => run_setup(cli.json, reset, cli.state_dir),
        Some(Command::Device) => not_implemented(
            cli.json,
            "device",
            "the device listener is P8: 127.0.0.1:7341 is not bound by this build",
        ),
        Some(Command::Eval) => not_implemented(
            cli.json,
            "eval",
            "the evaluation harness is not implemented: this build ships serve, probe, setup and doctor",
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

/// Resolve the paths this process uses: `--state-dir`, then `$HOME`.
fn resolve_paths(state_dir: Option<PathBuf>) -> Result<Paths, String> {
    match state_dir {
        Some(root) => Ok(Paths::for_root(root)),
        None => Paths::from_home().map_err(|error| error.to_string()),
    }
}

fn run_serve(state_dir: Option<PathBuf>, allow_full: bool) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris serve: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let loaded = match LoadedConfig::load(paths.clone()) {
        Ok(loaded) => loaded,
        Err(message) => {
            eprintln!("eris serve: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    init_logging(loaded.toml().log_level());

    // The database is opened before the listener: a daemon that cannot reach its
    // state must not answer health checks.
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris serve: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let engine = SetupEngine::new(db, paths.clone());

    // First boot: reconcile the state against the credential table, then
    // generate the six-word passphrase exactly once. A daemon in `ready` never
    // touches the file and never prints the words.
    let state = match engine.reconcile_first_boot() {
        Ok(state) => state,
        Err(error) => {
            eprintln!("eris serve: cannot read the setup state: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    match engine.ensure_first_run() {
        Ok(run) => {
            if let Some(passphrase) = run.generated {
                let user = std::env::var("USER").unwrap_or_else(|_| "operator".to_owned());
                let host = hostname();
                print!(
                    "{}",
                    first_run_block(&passphrase, &run.first_run_path, &user, &host, 7340)
                );
                let _ = io::stdout().flush();
            } else if run.state == eris_host::db::SetupState::Setup {
                print!("{}", pending_block(&run.first_run_path, 7340));
                let _ = io::stdout().flush();
            }
        }
        Err(error) => {
            // A filesystem that cannot hold the file is a hard stop: without the
            // file the operator has no passphrase and the wizard cannot be
            // claimed.
            eprintln!("eris serve: cannot prepare the first-run passphrase: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    }

    tracing::info!(
        db = %paths.db_file.display(),
        workspace = %loaded.toml().workspace_path().display(),
        setup_state = state.as_str(),
        allow_full,
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

    let state = AppState::new(engine, allow_full);
    match runtime.block_on(admin::serve(state)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("eris serve: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// `eris setup` reports progress; `eris setup --reset` reopens the wizard after
/// verifying the existing admin passphrase, which is read from stdin so it never
/// appears in `ps` output or a shell history file.
fn run_setup(json: bool, reset: bool, state_dir: Option<PathBuf>) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris setup: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris setup: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let engine = SetupEngine::new(db, paths);

    if !reset {
        let wizard = match engine.wizard() {
            Ok(wizard) => wizard,
            Err(error) => {
                eprintln!("eris setup: {error}");
                return ExitCode::from(EXIT_FAILURE);
            }
        };
        if json {
            let steps: Vec<serde_json::Value> = wizard
                .steps
                .iter()
                .map(|status| {
                    serde_json::json!({
                        "id": status.step.as_str(),
                        "optional": status.optional,
                        "completed_at": status.completed_at,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "state": wizard.state.as_str(),
                    "next_step": wizard.next_step.map(|step| step.as_str()),
                    "steps": steps,
                    "url": "http://127.0.0.1:7340/setup",
                })
            );
        } else {
            println!("state: {}", wizard.state.as_str());
            println!(
                "next step: {}",
                wizard.next_step.map_or("none", |step| step.as_str())
            );
            for status in &wizard.steps {
                println!(
                    "  {:<12} {:<8} {}",
                    status.step.as_str(),
                    if status.optional {
                        "optional"
                    } else {
                        "required"
                    },
                    status.completed_at.as_deref().unwrap_or("open")
                );
            }
            println!("wizard: http://127.0.0.1:7340/setup");
            println!(
                "the setup passphrase, if still pending, is in {}",
                engine.first_run_path().display()
            );
        }
        return ExitCode::SUCCESS;
    }

    // The recovery path. The passphrase is read from stdin and never echoed.
    eprintln!("eris setup --reset: enter the existing admin passphrase on stdin");
    let mut line = String::new();
    let stdin = io::stdin();
    if stdin.lock().read_line(&mut line).is_err() || line.trim().is_empty() {
        eprintln!("eris setup --reset: no passphrase on stdin");
        return ExitCode::from(EXIT_FAILURE);
    }
    match engine.reset(line.trim()) {
        Ok(run) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ok",
                        "state": "setup",
                        "first_run_file": run.first_run_path.display().to_string(),
                        "url": "http://127.0.0.1:7340/setup",
                    })
                );
            } else if let Some(passphrase) = run.generated {
                let user = std::env::var("USER").unwrap_or_else(|_| "operator".to_owned());
                let host = hostname();
                print!(
                    "{}",
                    first_run_block(&passphrase, &run.first_run_path, &user, &host, 7340)
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("eris setup --reset: {error}");
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

/// The host name, for the SSH tunnel line in the first-run block.
fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|text| text.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this host".to_owned())
}

/// Install a stderr logger at the configured level.
fn init_logging(level: LogLevel) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(level.as_tracing())
        .with_target(false)
        .with_writer(io::stderr)
        .try_init();
}
