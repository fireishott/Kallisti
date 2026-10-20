//! `eris` - the Kallisti 1.0 daemon binary.
//!
//! Command surface as of P8. `serve` is the default: with no subcommand, the
//! daemon starts. `probe` is the P4 provider proof, `doctor` reports what it
//! verified, `setup` drives the first-run recovery path, `device token` is the
//! documented test affordance for acceptance runs, `eval` is still a placeholder
//! that exits non-zero on purpose (a placeholder that exits 0 looks like a
//! working feature to every script that calls it), and `version` prints one line.
//!
//! `serve` is the only command that binds a port. The admin addresses come from
//! `admin.listen` in `config.toml` plus every `--admin-listen` flag, defaulting to
//! `127.0.0.1:7340`; the device port (7341) is still a literal in
//! `device/mod.rs` and is bound only with `--device-listen` or when the dashboard
//! switch says so. Nothing here installs a service or touches another process on
//! the host.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use eris_host::admin::AppState;
use eris_host::auth::{AuthStore, ScopeSet, TokenOrigin, TokenService};
use eris_host::backup::{self, BackupError, CreateRequest, RestoreRequest};
use eris_host::config::{LoadedConfig, LogLevel, Paths};
use eris_host::db::Db;
use eris_host::onboarding::{SetupEngine, first_run_block, pending_block};
use eris_host::persona::PersonaConfig;
use eris_host::{admin, doctor, probe};

/// Exit code for a failure.
const EXIT_FAILURE: u8 = 1;
/// Exit code for a subcommand that is not implemented in this build.
const EXIT_NOT_IMPLEMENTED: u8 = 2;
/// Exit code for a device the command refuses to mint for: unknown or revoked.
const EXIT_DEVICE_REFUSED: u8 = 3;

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
    #[arg(long, global = true, value_name = "PATH", alias = "into")]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the admin listener: every address in `admin.listen`, defaulting to
    /// 127.0.0.1:7340. The default subcommand.
    Serve {
        /// Allow `full` sandbox mode to be stored. Without this flag, `full` is
        /// refused by onboarding and by the dashboard, and only a restart with
        /// the flag can change that.
        #[arg(long)]
        allow_full: bool,
        /// Bind the device listener on 127.0.0.1:7341. Without this flag the port
        /// is bound only when the dashboard switch is on.
        #[arg(long)]
        device_listen: bool,
        /// Also serve the admin UI on this address. Repeatable, and additive:
        /// every address here is served in addition to everything in
        /// `admin.listen`, so `--admin-listen 0.0.0.0:7340` needs no config
        /// edit. Any literal `ip:port` is accepted, including a LAN address, a
        /// tailnet address and `0.0.0.0`; a hostname is refused before anything
        /// binds.
        #[arg(long, value_name = "IP:PORT", action = clap::ArgAction::Append)]
        admin_listen: Vec<String>,
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
    /// Device helpers. Nothing here binds a port; the listener is `serve`.
    Device {
        #[command(subcommand)]
        command: Option<DeviceCommand>,
    },
    /// Write an archive of all state, or list one with `--list`.
    Backup {
        /// Absolute path of the archive to write, or to read with `--list`.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// With `--list`: the archive to read, as `eris backup --list <path>`.
        #[arg(value_name = "ARCHIVE")]
        archive: Option<PathBuf>,
        /// Include the sandbox workspace. It is user data and is off by default.
        #[arg(long)]
        include_workspace: bool,
        /// Re-read the archive and verify every member after writing it.
        #[arg(long)]
        verify: bool,
        /// Read the manifest of an existing archive. Writes nothing.
        #[arg(long)]
        list: bool,
        /// Allow a destination under the system temporary directory.
        #[arg(long)]
        allow_tmp: bool,
    },
    /// Restore state from an archive. Destructive: takes a safety copy first.
    Restore {
        /// The archive to restore from.
        #[arg(long = "from", value_name = "PATH")]
        from: PathBuf,
        /// Replace existing state. Required when the target is not empty.
        #[arg(long)]
        force: bool,
        /// Verify and report what would change, writing nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Review the writes a turn parked, and settle one.
    Approvals {
        #[command(subcommand)]
        command: ApprovalsCommand,
    },
    /// Report what this process verified: config, database, provider reachability.
    Doctor,
    /// Placeholder. The evaluation harness is not part of this build.
    Eval,
    /// Print the version line.
    Version,
}

/// The `device` subcommands.
#[derive(Debug, Subcommand)]
enum DeviceCommand {
    /// TEST-ONLY: mint one access token for an existing device and print it.
    Token {
        /// The device id to mint for. It must already exist and not be revoked.
        #[arg(long, value_name = "DEVICE_ID")]
        id: String,
        /// Print the token and nothing else on stdout.
        #[arg(long)]
        print: bool,
    },
}

/// The `approvals` subcommands.
#[derive(Debug, Subcommand)]
enum ApprovalsCommand {
    /// List the writes a turn parked and nobody has answered.
    List,
    /// Approve one and run it now. Writes only if the sandbox allows it.
    Approve {
        /// The approval id shown by `approvals list`.
        id: String,
    },
    /// Refuse one. Nothing is written and the row is settled.
    Deny {
        /// The approval id shown by `approvals list`.
        id: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        // `serve` is the default: no subcommand starts the daemon.
        None => run_serve(cli.state_dir, false, false, Vec::new()),
        Some(Command::Serve {
            allow_full,
            device_listen,
            admin_listen,
        }) => run_serve(cli.state_dir, allow_full, device_listen, admin_listen),
        Some(Command::Probe { prompt }) => run_probe(&prompt),
        Some(Command::Setup { reset }) => run_setup(cli.json, reset, cli.state_dir),
        Some(Command::Device { command }) => match command {
            Some(DeviceCommand::Token { id, print }) => {
                run_device_token(cli.json, &id, print, cli.state_dir)
            }
            None => not_implemented(
                cli.json,
                "device",
                "device has one subcommand: `device token --id <device_id> [--print]`. \
                 The listener itself is `eris serve --device-listen`",
            ),
        },
        Some(Command::Approvals { command }) => run_approvals(cli.json, cli.state_dir, command),
        Some(Command::Eval) => not_implemented(
            cli.json,
            "eval",
            "the evaluation harness is not implemented: this build ships serve, probe, setup, device and doctor",
        ),
        Some(Command::Backup {
            out,
            archive,
            include_workspace,
            verify,
            list,
            allow_tmp,
        }) => run_backup(
            cli.json,
            cli.state_dir,
            BackupArgs {
                out,
                archive,
                include_workspace,
                verify,
                list,
                allow_tmp,
            },
        ),
        Some(Command::Restore {
            from,
            force,
            dry_run,
        }) => run_restore(cli.json, cli.state_dir, &from, force, dry_run),
        Some(Command::Doctor) => run_doctor(cli.json),
        Some(Command::Version) => {
            println!("eris {} build {}", admin::VERSION, admin::BUILD);
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

/// `eris approvals` - review the writes a turn parked, and settle one.
///
/// Approving runs the call before it settles the row, so a row that the sandbox
/// still refuses stays pending instead of reading as approved work that never
/// happened.
fn run_approvals(json: bool, state_dir: Option<PathBuf>, command: ApprovalsCommand) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris approvals: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris approvals: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    match command {
        ApprovalsCommand::List => {
            let waiting = match eris_host::approvals::pending(&db) {
                Ok(waiting) => waiting,
                Err(error) => {
                    eprintln!("eris approvals: {error}");
                    return ExitCode::from(EXIT_FAILURE);
                }
            };
            if json {
                let rows: Vec<serde_json::Value> = waiting
                    .iter()
                    .map(|row| {
                        serde_json::json!({
                            "id": row.id,
                            "conversation": row.conversation,
                            "tool": row.tool,
                            "arguments": row.arguments,
                            "created_at": row.created_at,
                            "advice": row.advice,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "pending": rows }));
            } else if waiting.is_empty() {
                println!("no writes are waiting for approval");
            } else {
                for row in &waiting {
                    println!("{}  {}  {}", row.id, row.tool, row.arguments);
                    // Advice is printed under the call, never in place of it: the
                    // decision is still the operator's.
                    if let Some(advice) = &row.advice {
                        println!("    advice: {advice}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        ApprovalsCommand::Deny { id } => {
            match eris_host::approvals::decide(
                &db,
                &id,
                eris_host::approvals::Decision::Denied,
                "operator",
            ) {
                Ok(true) => {
                    println!("denied {id}");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    eprintln!("eris approvals: {id} is not pending, so nothing changed");
                    ExitCode::from(EXIT_FAILURE)
                }
                Err(error) => {
                    eprintln!("eris approvals: {error}");
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ApprovalsCommand::Approve { id } => {
            let row = match eris_host::approvals::get(&db, &id) {
                Ok(Some(row)) => row,
                Ok(None) => {
                    eprintln!("eris approvals: no approval with id {id}");
                    return ExitCode::from(EXIT_FAILURE);
                }
                Err(error) => {
                    eprintln!("eris approvals: {error}");
                    return ExitCode::from(EXIT_FAILURE);
                }
            };

            let arguments: serde_json::Value = match serde_json::from_str(&row.arguments) {
                Ok(arguments) => arguments,
                Err(error) => {
                    eprintln!(
                        "eris approvals: {} has unreadable arguments: {error}",
                        row.id
                    );
                    return ExitCode::from(EXIT_FAILURE);
                }
            };

            let engine = SetupEngine::new(db.clone(), paths.clone());
            let workspace = engine
                .workspace_path()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let mode = engine
                .permissions()
                .map(|permissions| permissions.sandbox_mode)
                .unwrap_or(eris_host::onboarding::permissions::SandboxMode::ReadOnly);
            let sandbox = eris_host::tools::Sandbox::new(workspace, mode);

            // A namespaced name is an MCP tool: approving it has to reach the
            // server, not the local file executor, or an approval would report a
            // refusal for a tool that exists.
            // `display` is what the transcript keeps: a server's own words, not
            // this run's JSON envelope, or a run log entry reads as escaped soup.
            let (result, display) =
                if eris_host::mcp::protocol::split_qualified(&row.tool).is_some() {
                    let servers = engine
                        .setting(eris_host::mcp::SETTING_SERVERS)
                        .ok()
                        .flatten()
                        .and_then(|raw| eris_host::mcp::parse(&raw).ok())
                        .unwrap_or_default();
                    let pool = eris_host::mcp::Pool::new(servers);
                    match pool.call(&row.tool, &arguments) {
                        Ok(outcome) => (
                            eris_host::tools::ToolResult::from_mcp(&row.tool, &outcome),
                            outcome.text,
                        ),
                        Err(error) => {
                            let refusal = eris_host::tools::ToolResult::refused(&error.message);
                            let text = error.message.clone();
                            (refusal, text)
                        }
                    }
                } else {
                    let result = eris_host::tools::execute(&row.tool, &arguments, &sandbox);
                    let text = result.content.clone();
                    (result, text)
                };
            if result.is_error {
                eprintln!(
                    "eris approvals: {} was not run, so it stays pending: {}",
                    row.id, result.content
                );
                return ExitCode::from(EXIT_FAILURE);
            }

            // The transcript is rewritten before the row is settled, so a
            // decision that lands leaves a page that tells the truth. A zero here
            // means the turn never stored the call; that is reported, not hidden.
            let rewritten = eris_host::web::chat::apply_approval_result(
                &db,
                &row.id,
                &display,
                result.is_error,
            )
            .unwrap_or(0);

            match eris_host::approvals::decide(
                &db,
                &id,
                eris_host::approvals::Decision::Approved,
                "operator",
            ) {
                Ok(true) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "id": row.id,
                                "status": "approved",
                                "result": result.content,
                            })
                        );
                    } else {
                        println!("approved {id}: {}", result.content);
                        if rewritten == 0 {
                            eprintln!(
                                "eris approvals: the turn did not store this call, so the transcript \
                                 still shows it as parked"
                            );
                        }
                    }
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    eprintln!(
                        "eris approvals: {} was already settled; the work ran but no row changed",
                        row.id
                    );
                    ExitCode::from(EXIT_FAILURE)
                }
                Err(error) => {
                    eprintln!("eris approvals: {error}");
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
    }
}

fn run_serve(
    state_dir: Option<PathBuf>,
    allow_full: bool,
    device_listen: bool,
    admin_listen: Vec<String>,
) -> ExitCode {
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

    // The listener list is resolved from the config file plus every flag, and
    // validated before anything binds: a refused address must fail with a
    // message about the address, never with "address already in use" from a
    // listener that is already up, and it must never open a socket.
    let configured = loaded.toml().admin().listen().to_vec();
    let listeners = match admin::listen::resolve_listeners(&configured, &admin_listen) {
        Ok(listeners) => listeners,
        Err(error) => {
            eprintln!("eris serve: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let access = loaded.toml().admin().access().clone();
    if let Some(url) = access.public_url.as_deref() {
        tracing::info!(%url, "public URL configured");
    }

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

    // A `streaming` row means a turn was writing it when the process went away.
    // At boot there is no such turn, so those rows are settled now rather than
    // rendered as a bubble that fills in forever.
    match eris_host::web::chat::fail_orphaned(&db) {
        Ok(0) => {}
        Ok(count) => eprintln!("eris serve: settled {count} interrupted chat row(s)"),
        Err(error) => eprintln!("eris serve: could not settle interrupted chat rows: {error}"),
    }

    // The same argument, for the same reason: a `queued` or `running` subagent at
    // boot belongs to a task this process never started. Left alone it would spend
    // the concurrency cap forever and sit in the top bar as work still happening.
    match eris_host::subagents::fail_orphaned(&db) {
        Ok(0) => {}
        Ok(count) => eprintln!("eris serve: settled {count} interrupted subagent(s)"),
        Err(error) => eprintln!("eris serve: could not settle interrupted subagents: {error}"),
    }

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
                    first_run_block(
                        &passphrase,
                        &run.first_run_path,
                        &user,
                        &host,
                        primary_port(&listeners)
                    )
                );
                let _ = io::stdout().flush();
            } else if run.state == eris_host::db::SetupState::Setup {
                print!(
                    "{}",
                    pending_block(&run.first_run_path, primary_port(&listeners))
                );
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

    let state = AppState::new(engine, allow_full)
        .with_access(access)
        .with_persona(PersonaConfig::from_toml(loaded.toml().persona(), &paths));

    // The device listener is opt-in twice over: the flag, or the switch the
    // operator last saved in the dashboard. Either one binds 127.0.0.1:7341; the
    // flag exists so an acceptance run does not have to drive the wizard first.
    // Where the device listener binds. A phone cannot reach loopback, so this is
    // the setting that makes pairing possible off-host. Unset means loopback.
    let device_bind = state
        .engine
        .setting(eris_host::device::SETTING_LISTEN)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .and_then(|list| eris_host::admin::listen::parse_listen_list(&list).ok());
    if let Some(addresses) = device_bind {
        tracing::info!(?addresses, "device listener bind from settings");
        state.device.set_bind(addresses);
    }

    let device_setting = state.engine.device_listener_enabled().unwrap_or(false);
    let device_enabled = device_listen || device_setting;
    state.device.set_accepting(device_enabled);
    tracing::info!(
        device_listen,
        device_setting,
        device_enabled,
        "device listener decision"
    );

    match runtime.block_on(async move {
        if device_enabled {
            state.device.start();
        }
        admin::serve_listeners(state, listeners).await
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("eris serve: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// The port the first-run block prints: the first configured listener's port,
/// because that is the URL the operator is most likely to open.
fn primary_port(listeners: &[std::net::SocketAddr]) -> u16 {
    listeners
        .first()
        .map(std::net::SocketAddr::port)
        .unwrap_or(7340)
}

/// `eris device token --id <device_id> [--print]`: mint one access token for an
/// existing device.
///
/// **This is a local, owner-only test affordance for acceptance runs, not a
/// supported way to distribute credentials.** It exists because P10's acceptance
/// chain has to drive `/v1/turns` and `/v1/.../events` as a paired device, and
/// the honest way to pair over HTTP is to redeem a pairing code that a human read
/// off the wizard - a code that has to be delivered out of band, which a scripted
/// run on one host cannot do end to end. Minting through [`TokenService`] keeps
/// every storage rule intact: only the Argon2id verifier reaches the database and
/// the plaintext exists once, on stdout.
///
/// It deliberately cannot do the things a credential-distribution tool would:
/// there is no way to list tokens, no way to read a token back, no way to mint
/// for more than the one `--id` given, and no way to mint for a device that is
/// unknown or revoked. Exit code 3 means "refused", so a script can tell a bad id
/// from a broken database.
fn run_device_token(
    json: bool,
    device_id: &str,
    print: bool,
    state_dir: Option<PathBuf>,
) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris device token: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris device token: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };

    let store = AuthStore::new(db.clone());
    match store.device(device_id) {
        Ok(Some(device)) => {
            if device.revoked_at.is_some() {
                eprintln!(
                    "eris device token: device {device_id} is revoked; pair the device again instead"
                );
                return ExitCode::from(EXIT_DEVICE_REFUSED);
            }
        }
        Ok(None) => {
            eprintln!("eris device token: no device with id {device_id} in this state directory");
            return ExitCode::from(EXIT_DEVICE_REFUSED);
        }
        Err(error) => {
            eprintln!("eris device token: cannot read the device: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    }

    let tokens = TokenService::new(db);
    let minted =
        match tokens.mint_access(device_id, ScopeSet::device_default(), TokenOrigin::Device) {
            Ok(minted) => minted,
            Err(error) => {
                eprintln!("eris device token: cannot mint: {error}");
                return ExitCode::from(EXIT_DEVICE_REFUSED);
            }
        };
    // The audit row carries the device id and never the value.
    let _ = store.audit("cli", "device.token_minted", Some(device_id));
    let expires_at = eris_host::auth::format_timestamp(minted.expires_at());

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "device_id": device_id,
                "token": minted.token(),
                "expires_at": expires_at,
            })
        );
    } else {
        if !print {
            eprintln!(
                "eris device token: minted an access token for {device_id} (expires {expires_at})"
            );
        }
        println!("{}", minted.token());
    }
    ExitCode::SUCCESS
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
    // The recovery path prints the URL a browser should open. It comes from the
    // same config the daemon reads; a config that cannot be read falls back to
    // the documented default rather than blocking a reset.
    let port = LoadedConfig::load(paths.clone())
        .map(|loaded| primary_port(loaded.toml().admin().listen()))
        .unwrap_or(7340);
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
                    "url": format!("http://127.0.0.1:{port}/setup"),
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
            println!("wizard: http://127.0.0.1:{port}/setup");
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
                        "url": format!("http://127.0.0.1:{port}/setup"),
                    })
                );
            } else if let Some(passphrase) = run.generated {
                let user = std::env::var("USER").unwrap_or_else(|_| "operator".to_owned());
                let host = hostname();
                print!(
                    "{}",
                    first_run_block(&passphrase, &run.first_run_path, &user, &host, port)
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
    let paths = match Paths::from_home() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("eris probe: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let config = match eris_host::config::KallistiConfig::load(&paths.env_file) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("eris probe: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    // A probe carries the same identity the daemon would: it is the same
    // `[persona]` table and the same file, resolved against the real paths.
    let persona = match eris_host::config::TomlConfig::load(&paths.toml_file) {
        Ok(toml) => PersonaConfig::from_toml(toml.persona(), &paths),
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

    let result = runtime.block_on(probe::run(&config, &persona, prompt, &mut out, &mut diag));

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

/// The `backup` flags, gathered so the handler signature stays readable.
struct BackupArgs {
    out: Option<PathBuf>,
    archive: Option<PathBuf>,
    include_workspace: bool,
    verify: bool,
    list: bool,
    allow_tmp: bool,
}

/// `eris backup`: write an archive, or read one with `--list`.
///
/// Every path prints exactly one JSON object under `--json`, including the
/// failures: a 200-style success with an empty body would be indistinguishable
/// from a bug to every script that calls this.
fn run_backup(json: bool, state_dir: Option<PathBuf>, args: BackupArgs) -> ExitCode {
    if args.list {
        let target = match (args.out.clone(), args.archive.clone()) {
            (Some(out), None) => out,
            (None, Some(archive)) => archive,
            (Some(_), Some(_)) => {
                return usage_failure(
                    json,
                    "backup",
                    "give either --out <path> or a positional <path> with --list, not both",
                );
            }
            (None, None) => {
                return usage_failure(
                    json,
                    "backup",
                    "--list needs the archive: `eris backup --list --out <path>` or `eris backup --list <path>`",
                );
            }
        };
        return match backup::open(&target) {
            Ok(opened) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "archive": target.display().to_string(),
                            "archive_bytes": opened.archive_bytes,
                            "archive_sha256": opened.archive_sha256,
                            "format_version": opened.manifest.format_version,
                            "schema_version": opened.manifest.schema_version,
                            "eris_version": opened.manifest.eris_version,
                            "created_at": opened.manifest.created_at,
                            "host": opened.manifest.host,
                            "member_count": opened.manifest.members.len(),
                            "members": opened.manifest.members.iter().map(backup::Member::to_json).collect::<Vec<_>>(),
                            "excluded": opened.manifest.excluded.iter().map(backup::Excluded::to_json).collect::<Vec<_>>(),
                            "wal_partial_bytes": opened.manifest.wal_partial_bytes,
                            "manifest_sha256": opened.manifest.manifest_sha256,
                        })
                    );
                } else {
                    println!(
                        "{}: {} members, schema {}, written {} by eris {}",
                        target.display(),
                        opened.manifest.members.len(),
                        opened.manifest.schema_version,
                        opened.manifest.created_at,
                        opened.manifest.eris_version
                    );
                    for member in &opened.manifest.members {
                        println!(
                            "  {:<16} {:>10}  {}",
                            member.kind, member.bytes, member.path
                        );
                    }
                    for excluded in &opened.manifest.excluded {
                        println!("  excluded  {}  ({})", excluded.path, excluded.reason);
                    }
                }
                ExitCode::SUCCESS
            }
            Err(error) => backup_failure(json, "backup --list", &error),
        };
    }

    let Some(out) = args.out else {
        return usage_failure(
            json,
            "backup",
            "--out <absolute-path> is required: `eris backup --out ~/kallisti-backup.bk`",
        );
    };
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => return backup_failure_from_message(json, "backup", &message),
    };
    let request = CreateRequest {
        out,
        include_workspace: args.include_workspace,
        verify: args.verify,
        allow_tmp: args.allow_tmp,
        paths,
    };
    match backup::create(&request) {
        Ok(report) => {
            if json {
                println!("{}", report.to_json());
            } else {
                println!(
                    "backup written: {} ({} bytes, sha256 {})",
                    report.archive.display(),
                    report.archive_bytes,
                    report.archive_sha256
                );
                for member in &report.members {
                    println!(
                        "  {:<16} {:>10}  {}",
                        member.kind, member.bytes, member.path
                    );
                }
                for excluded in &report.excluded {
                    println!("  excluded  {}  ({})", excluded.path, excluded.reason);
                }
                if report.wal_partial_bytes > 0 {
                    println!(
                        "  note: {} bytes of live WAL tail were dropped because the daemon held the session lock",
                        report.wal_partial_bytes
                    );
                }
                println!(
                    "  schema {}  verified {}",
                    report.schema_version,
                    if report.verified {
                        "yes"
                    } else {
                        "no (--verify not given)"
                    }
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => backup_failure(json, "backup", &error),
    }
}

/// `eris restore`: verify, take a safety copy, then place.
fn run_restore(
    json: bool,
    state_dir: Option<PathBuf>,
    from: &std::path::Path,
    force: bool,
    dry_run: bool,
) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => return backup_failure_from_message(json, "restore", &message),
    };
    let request = RestoreRequest {
        from: from.to_path_buf(),
        force,
        dry_run,
        paths,
    };
    match backup::restore(&request) {
        Ok(report) => {
            if json {
                println!("{}", report.to_json());
            } else if report.dry_run {
                println!(
                    "dry run: would restore {} members from {} (schema {})",
                    report.members,
                    report.archive.display(),
                    report.schema_version
                );
                for path in &report.would_write {
                    println!("  would write  {path}");
                }
                println!("  database quick_check: {}", report.quick_check);
            } else {
                println!(
                    "restored {} members from {} (schema {})",
                    report.members,
                    report.archive.display(),
                    report.schema_version
                );
                if let Some(safety) = &report.safety_copy {
                    println!("  previous state moved to {}", safety.display());
                }
                for path in &report.placed {
                    println!("  placed  {path}");
                }
                println!(
                    "  database quick_check: {}  sha256 {}",
                    report.quick_check,
                    report.db_sha256.as_deref().unwrap_or("unknown")
                );
                println!(
                    "  restart the daemon to reopen the restored database: systemctl --user restart kallisti-eris"
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => backup_failure(json, "restore", &error),
    }
}

/// A `backup`/`restore` failure: one JSON object under `--json`, exit by class.
fn backup_failure(json: bool, command: &str, error: &BackupError) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": false,
                "error": {
                    "code": error.code(),
                    "message": error.to_string(),
                },
            })
        );
    } else {
        eprintln!("eris {command}: {error}");
    }
    ExitCode::from(error.exit_code())
}

/// A usage failure that never reached [`BackupError`].
fn usage_failure(json: bool, command: &str, message: &str) -> ExitCode {
    backup_failure(
        json,
        command,
        &BackupError::Usage {
            message: message.to_owned(),
        },
    )
}

/// A path-resolution failure, which is an I/O-class failure and not a usage one.
fn backup_failure_from_message(json: bool, command: &str, message: &str) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": false,
                "error": { "code": "config", "message": message },
            })
        );
    } else {
        eprintln!("eris {command}: {message}");
    }
    ExitCode::from(backup::EXIT_FAILURE)
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
