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
use std::time::Duration;

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

/// How long shutdown waits for blocking work after the listeners have stopped.
///
/// Dropping a multi-thread runtime waits for every `spawn_blocking` task with no
/// bound, and a turn runs its whole provider loop on one of those. Five seconds
/// is enough for an in-flight database transaction to settle and far short of
/// systemd's 90-second `TimeoutStopSec`, so a turn that is still streaming at
/// shutdown is abandoned rather than holding the stop until it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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
    /// Report the standing roster: every named agent, its role and its state.
    /// Read-only: the dashboard is what edits a row.
    Roster,
    /// Report the token budget: what the window spent, the soft cap, and what is
    /// left. The same numbers the `budget` tool answers a turn with.
    Budget {
        /// The window to count: `24h`, `7d` or `all`. Defaults to `all`.
        #[arg(long, default_value = "all")]
        window: String,
        /// One conversation to report on. Omitted reports every one.
        #[arg(long, value_name = "ID")]
        conversation: Option<String>,
    },
    /// Report the webhook triggers: name, enabled, token prefix, fire count and
    /// when each last fired. Never the hash, and never a token.
    Trigger {
        /// The action. `list` is the only one, and it is the default.
        #[arg(default_value = "list")]
        action: String,
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
    /// The operator's own memory writes. Nothing here reaches a provider.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
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

/// The `memory` subcommands.
///
/// `add` is the operator's own door into the two tier-1 files: rows written here
/// carry `origin=operator`, the only origin the recall predicate trusts, so it is
/// the one way an entry becomes recallable without a review step.
///
/// `pending` and `accept` are the review step. A consolidation sweep stores a
/// proposal as an untrusted row, and recall reads only trusted ones, so without
/// these two a proposal is stored, inspectable and permanently invisible. `accept`
/// is the only code in the tree that moves `trusted` off zero.
#[derive(Debug, Subcommand)]
enum MemoryCommand {
    /// Append one entry to a tier-1 file, as the operator.
    Add {
        /// Which file: `memory` for the assistant's own notes, `user` for what
        /// it knows about the operator.
        #[arg(long, value_name = "TARGET", default_value = "memory")]
        target: String,
        /// The entry. It is stored as one bullet line, the same shape the tool
        /// writes, and the same cap and duplicate rules apply.
        #[arg(long, value_name = "TEXT")]
        text: String,
    },
    /// List the entries nothing has accepted yet, oldest first.
    Pending {
        /// The most rows to list.
        #[arg(long, value_name = "N", default_value_t = 50)]
        limit: usize,
    },
    /// Accept one entry: promote it to trusted and write it into its own file.
    Accept {
        /// The id shown by `memory pending`.
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
        Some(Command::Memory { command }) => match command {
            MemoryCommand::Add { target, text } => {
                run_memory_add(cli.json, &target, &text, cli.state_dir)
            }
            MemoryCommand::Pending { limit } => run_memory_pending(cli.json, limit, cli.state_dir),
            MemoryCommand::Accept { id } => run_memory_accept(cli.json, &id, cli.state_dir),
        },
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
        Some(Command::Roster) => run_roster(cli.json, cli.state_dir),
        Some(Command::Budget {
            window,
            conversation,
        }) => run_budget(&window, conversation.as_deref(), cli.json, cli.state_dir),
        Some(Command::Trigger { action }) => run_trigger(&action, cli.json, cli.state_dir),
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
            // One execution path: the same function a paired device calls, so an
            // approval means the same thing whichever surface answered it.
            match eris_host::approvals::approve_and_run(&db, &paths, &id, "operator") {
                Ok(eris_host::approvals::Ran::Done { content, rewritten }) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "id": id,
                                "status": "approved",
                                "result": content,
                            })
                        );
                    } else {
                        println!("approved {id}: {content}");
                        if rewritten == 0 {
                            eprintln!(
                                "eris approvals: the turn did not store this call, so the transcript \
                                 still shows it as parked"
                            );
                        }
                    }
                    ExitCode::SUCCESS
                }
                Ok(eris_host::approvals::Ran::Refused(message)) => {
                    eprintln!("eris approvals: {id} was not run, so it stays pending: {message}");
                    ExitCode::from(EXIT_FAILURE)
                }
                Ok(eris_host::approvals::Ran::AlreadySettled) => {
                    eprintln!("eris approvals: {id} is not pending, so nothing changed");
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

/// `eris memory add` - append one entry to a tier-1 file, as the operator.
///
/// One writer, not two: this goes through `selfmemory::Access`, the same door
/// `memory_manage` uses, so the cap, the duplicate rule, the instruction and
/// credential scans, the file format and the `mem_entries` mirror are all the
/// code the tool path already runs. The only difference is the origin, which is
/// what makes the row recallable.
///
/// Without a database this still writes the file, exactly as a lane with no
/// mirror does, and says so rather than reporting a projection it did not make.
fn run_memory_add(json: bool, target: &str, text: &str, state_dir: Option<PathBuf>) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris memory add: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let Some(target) = eris_host::selfmemory::Target::parse(target) else {
        eprintln!(
            "eris memory add: `{target}` is not a target; it must be `memory` or `user`, so \
             nothing was written"
        );
        return ExitCode::from(EXIT_FAILURE);
    };
    // The database is opened before the file is touched: a row this command
    // cannot record is provenance the operator would not get back, and the file
    // is the authority everywhere else in this lane.
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris memory add: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let access = eris_host::selfmemory::Access::new(&paths.state_dir).with_db(db);
    let result = access.add_as(target, text, eris_host::mem::Origin::Operator);

    if result.is_error {
        // The content is the tool result's JSON, whose `error` field is the
        // sentence the operator needs: it already names the cap, the duplicate or
        // the marker that fired. A body without one is printed whole rather than
        // swallowed.
        let message = serde_json::from_str::<serde_json::Value>(&result.content)
            .ok()
            .and_then(|body| {
                body.get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| result.content.clone());
        eprintln!("eris memory add: {message}");
        return ExitCode::from(EXIT_FAILURE);
    }
    if json {
        println!("{}", result.content);
    } else {
        println!("added one entry to {}", target.file());
    }
    ExitCode::SUCCESS
}

/// `eris memory pending` - list the rows nothing has accepted yet.
///
/// This is the other half of consolidation. A sweep proposes by inserting an
/// untrusted row, and recall reads only trusted ones, so a proposal that no
/// surface lists is a proposal nobody can act on. The listing is the whole queue,
/// oldest first, with the fields an operator needs to decide: the id to accept,
/// where it came from, which file it would land in, and the text itself.
fn run_memory_pending(json: bool, limit: usize, state_dir: Option<PathBuf>) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris memory pending: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris memory pending: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let waiting = match eris_host::mem::pending_entries(&db, limit) {
        Ok(waiting) => waiting,
        Err(error) => {
            eprintln!("eris memory pending: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    if json {
        let rows: Vec<serde_json::Value> = waiting
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id,
                    "origin": row.origin,
                    "target": row.target,
                    "entry": row.entry,
                    "created_at": row.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "pending": rows }));
    } else if waiting.is_empty() {
        println!("no entries are waiting to be accepted");
    } else {
        for row in &waiting {
            println!("{}  {}  {}  {}", row.id, row.origin, row.target, row.entry);
        }
    }
    ExitCode::SUCCESS
}

/// `eris memory accept` - add one proposal to the tier-1 file and trust it.
///
/// The order here is the whole point, and it is file first, flag second.
///
/// The file is the authority in this lane. `write_as` is the only writer of the
/// two tier-1 files, so it is what enforces the cap, the duplicate rule and the
/// instruction and credential scans. A refusal there means the text is not in the
/// file's live set, and a row marked trusted whose text never landed there is
/// exactly the disagreement this lane exists to prevent: the table would report a
/// memory the prompt block does not carry, and no read would ever notice. So the
/// file is written first and a refusal returns before the row is touched. Nothing
/// is marked trusted, nothing is half-written, and the operator is told why.
///
/// The one refusal that is not a failure is the duplicate. A row the model wrote
/// through `memory_manage` is in the file and is still untrusted, so it appears in
/// `memory pending` and accepting it must not fail just because the text is
/// already where it belongs. That case is settled by asking the file directly:
/// when the entry is already in the live set, the file half of the accept is
/// already done and only the flag is left.
///
/// Only after the text is on disk does the row move, and it moves through
/// [`eris_host::mem::accept_entry`], whose predicate keeps the decision in one
/// statement. If that write fails the file holds the text and the row is still
/// untrusted, which is the harmless direction: the entry is readable in the file
/// and the next accept settles it.
///
/// The projection runs with `Origin::Operator`, because the origin names who
/// decided the entry belongs in memory and that decision is the operator's. It is
/// also what `mem::classify` trusts, so the row the file write mirrors cannot
/// disagree with the accept that preceded it. In practice that insert is a no-op:
/// the row already holds this `(target, content_hash)` pair, so the unique key
/// keeps one row and the accept is what changes it. The row keeps
/// `origin=consolidation`, which is the truthful provenance of an entry a sweep
/// proposed and a person kept.
fn run_memory_accept(json: bool, id: &str, state_dir: Option<PathBuf>) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris memory accept: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris memory accept: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let entry = match eris_host::mem::entry_by_id(&db, id) {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            eprintln!("eris memory accept: no entry has id {id}, so nothing was changed");
            return ExitCode::from(EXIT_FAILURE);
        }
        Err(error) => {
            eprintln!("eris memory accept: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    // Read before the write so the answer names the reason rather than reporting
    // a refusal from a write that could not have moved anything anyway.
    if entry.superseded_by.is_some() {
        eprintln!(
            "eris memory accept: {id} was superseded by another entry, so nothing was changed"
        );
        return ExitCode::from(EXIT_FAILURE);
    }
    if entry.trusted {
        eprintln!("eris memory accept: {id} is already trusted, so nothing was changed");
        return ExitCode::from(EXIT_FAILURE);
    }
    let Some(target) = eris_host::selfmemory::Target::parse(&entry.target) else {
        eprintln!(
            "eris memory accept: {id} names `{}`, which is not a target; it must be `memory` or \
             `user`, so nothing was written",
            entry.target
        );
        return ExitCode::from(EXIT_FAILURE);
    };

    // One writer, not two. `add_as` is the same append `memory add` and
    // `memory_manage` run: the same scan, the same duplicate rule, the same cap,
    // the same file format and the same `mem_entries` mirror. The accept step owns
    // a different question, which is whether the row is trusted, and it does not
    // get to write the file to answer it.
    let access = eris_host::selfmemory::Access::new(&paths.state_dir).with_db(db.clone());
    let written = access.add_as(target, &entry.entry, eris_host::mem::Origin::Operator);
    if written.is_error {
        // A refusal is only a failure when the text is not already in the file.
        // The membership test is the authority question itself, not a reading of
        // the refusal message, so the cap, the scan and the duplicate rule all
        // resolve the same way.
        if !file_lists(&access, target, &entry.entry) {
            let message = tool_message(&written.content);
            eprintln!(
                "eris memory accept: {id} was not accepted, so the row is still untrusted and {} \
                 is unchanged: {message}",
                target.file()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    }

    match eris_host::mem::accept_entry(&db, id) {
        Ok(true) => {}
        Ok(false) => {
            // The text is in the file and the row did not move. Another writer
            // settled it between the read and this statement. The file is correct
            // either way; the row is not, so this is reported as a failure rather
            // than dressed up as an accept.
            eprintln!(
                "eris memory accept: {} has the entry, but {id} did not move, so it is still \
                 untrusted",
                target.file()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
        Err(error) => {
            eprintln!(
                "eris memory accept: {} has the entry, but {id} could not be promoted: {error}",
                target.file()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "id": id,
                "status": "accepted",
                "target": target.label(),
                "file": target.file(),
                "entry": entry.entry,
            })
        );
    } else {
        println!("accepted {id} into {}", target.file());
    }
    ExitCode::SUCCESS
}

/// The sentence a tool result carries, or the whole body when it carries none.
fn tool_message(content: &str) -> String {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|body| {
            body.get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| content.to_owned())
}

/// Whether a tier-1 file already lists this entry.
///
/// Read through `Access::snapshot`, the same public read the phone and the memory
/// page use, rather than by parsing the file here: the bullet format belongs to
/// `selfmemory`, and a second parser in the CLI is a second way for the two to
/// disagree about what a line means.
///
/// The comparison is the one `add_as` makes, whitespace collapsed to single
/// spaces and case-insensitive. Anything looser would call an entry present that
/// the writer would then append a second time, and anything tighter would report
/// an entry missing that the writer refuses as a duplicate.
fn file_lists(
    access: &eris_host::selfmemory::Access,
    target: eris_host::selfmemory::Target,
    entry: &str,
) -> bool {
    let wanted = collapse(entry);
    let snapshot = access.snapshot();
    let Some(files) = snapshot.get("files").and_then(serde_json::Value::as_array) else {
        return false;
    };
    files
        .iter()
        .filter(|file| {
            file.get("target").and_then(serde_json::Value::as_str) == Some(target.label())
        })
        .filter_map(|file| file.get("entries").and_then(serde_json::Value::as_array))
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .any(|listed| collapse(listed).eq_ignore_ascii_case(&wanted))
}

/// One entry as a comparison key: single spaces, no surrounding whitespace.
fn collapse(entry: &str) -> String {
    entry.split_whitespace().collect::<Vec<_>>().join(" ")
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

    // Device turns are `jobs` rows, and they need the same treatment for a
    // sharper reason: the events route tails a broadcast channel once it has
    // replayed the log. With no worker left to append a terminal frame, a phone
    // resuming an interrupted turn waits for an ending that can never arrive.
    // Settling each row writes that ending in the same transaction.
    match eris_host::device::turns::fail_orphaned(&db) {
        Ok(0) => {}
        Ok(count) => eprintln!("eris serve: settled {count} interrupted turn(s)"),
        Err(error) => eprintln!("eris serve: could not settle interrupted turn(s): {error}"),
    }

    // And the same sweep for scheduled runs. A `running` row at boot is a run
    // whose turn this process never started: left alone it sits in the history
    // as work still happening, and its schedule keeps saying `running` with
    // nothing behind it.
    match eris_host::scheduler::fail_orphaned(&db) {
        Ok(0) => {}
        Ok(count) => eprintln!("eris serve: settled {count} interrupted scheduled run(s)"),
        Err(error) => eprintln!("eris serve: could not settle interrupted scheduled runs: {error}"),
    }

    // Every retired token verifier is one more Argon2 hash on every device
    // request, because verification scans the rows to answer "revoked" rather
    // than "unknown". A device that refreshes hourly retires two rows each
    // time, so the set only grows while the daemon runs; swept here, and again
    // after each rotation, it stays the tokens that are still live.
    match eris_host::auth::TokenService::new(db.clone())
        .prune_dead_tokens(eris_host::auth::token::RETIRED_GRACE)
    {
        Ok(0) => {}
        Ok(count) => eprintln!("eris serve: pruned {count} retired token verifier(s)"),
        Err(error) => eprintln!("eris serve: could not prune retired token verifiers: {error}"),
    }

    // A refused send used to leave the files it had already written, and their
    // `chat_attachments` rows, with no message to belong to. The request path no
    // longer writes anything it might have to take back, so what is left here is
    // the litter an earlier build made: swept once at boot, by its own age.
    match eris_host::web::chat::sweep_orphan_attachments(
        &db,
        eris_host::web::chat::ORPHAN_ATTACHMENT_GRACE_SECS,
    ) {
        Ok(0) => {}
        Ok(count) => eprintln!("eris serve: swept {count} unclaimed attachment(s)"),
        Err(error) => eprintln!("eris serve: could not sweep unclaimed attachments: {error}"),
    }

    let engine = SetupEngine::new(db, paths.clone());

    // The per-provider catalogues exist from this build on. An install that
    // predates them holds one probe under `provider.models`, and that probe was
    // taken against the endpoint its key reference names. Seeding that
    // provider's own row from it is a copy rather than a guess, and it is what
    // stops a second provider from inheriting the first one's list.
    match engine.migrate_provider_catalog() {
        Ok(seeded) if seeded.is_empty() => {}
        Ok(seeded) => eprintln!(
            "eris serve: seeded provider catalogue for {}",
            seeded.join(", ")
        ),
        Err(error) => {
            eprintln!("eris serve: could not seed the provider catalogue: {error}");
        }
    }

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

    // The credential resolver, installed once before anything can answer.
    //
    // Every `token:` reference in the registry resolves through it, and the store
    // it reads is the encrypted one under the same root the rest of the daemon
    // uses. `oauth.auto_refresh` comes from `config.toml` and is on unless the
    // operator wrote false, so a login grant that has reached its refresh lead is
    // renewed on use instead of failing the turn. What it cannot ever do is spend
    // a migrated grant: the refresher refuses one that came out of another
    // program's database before it sends anything. A host whose store cannot be
    // opened says so and keeps serving: an `env:` provider is unaffected, and the
    // one thing worse than a broken account is a daemon that will not start
    // because of one.
    // The same store is handed to two owners: the resolver, which turns a
    // reference into a credential for a turn, and the callback listener, which
    // seals a grant a login just produced. Both must be the *same* store - a
    // second one built from the same paths would work by luck and stop working
    // the moment either grew a cache - so the `Arc` is cloned rather than rebuilt.
    // The OAuth client ids and the callback port, read once, ahead of the
    // resolver: a refresh needs the same client id the login used, so the specs
    // are built from this file rather than from a second copy of the constants.
    // A file that does not parse is reported by path and the defaults stand,
    // because a callback on the wrong port is better than a daemon that will not
    // start.
    let clients = match eris_host::oauth::clients::load(&paths.config_dir) {
        Ok(clients) => clients,
        Err(error) => {
            tracing::error!(%error, "the OAuth client file was not understood; using the defaults");
            eris_host::oauth::clients::Clients::default()
        }
    };
    let mut oauth_store: Option<std::sync::Arc<dyn core_domain::TokenStore + Send + Sync>> = None;
    match eris_host::oauth::default_store(&paths) {
        Ok(store) => {
            oauth_store = Some(std::sync::Arc::clone(&store));
            let specs = eris_host::oauth::credential::refresh_specs(&clients);
            let installed = eris_host::oauth::credential::install(
                store,
                loaded.toml().oauth().auto_refresh(),
                specs,
            );
            tracing::info!(
                installed,
                auto_refresh = loaded.toml().oauth().auto_refresh(),
                "credential resolver installed"
            );
        }
        Err(error) => {
            tracing::error!(%error, "the credential store could not be opened; account references will be refused");
        }
    }
    tracing::info!(
        callback_port = clients.callback_port,
        configured = clients.source.is_some(),
        "OAuth callback listener"
    );

    let state = AppState::new(engine, allow_full)
        .with_access(access)
        .with_oauth(clients);
    // A host whose store would not open keeps the store the constructor built, so
    // the panel renders a state rather than failing the page. The login it seals
    // into goes nowhere, which is why the resolver above logs the same failure.
    let state = match oauth_store {
        Some(store) => state.with_oauth_store(store),
        None => state,
    };
    let state = state.with_persona(PersonaConfig::from_toml(loaded.toml().persona(), &paths));

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

    let scheduler_state = state.clone();
    // Publish a catalogue for any provider that has an endpoint and none yet.
    //
    // A restart is the one moment this process can afford to ask: the registry
    // may name a provider that was added while the daemon was running, and until
    // something calls its `/models` it contributes no options to any picker.
    // Spawned, not awaited, so a slow or refused endpoint delays nothing.
    let catalogue_state = state.clone();
    let outcome = runtime.block_on(async move {
        if device_enabled {
            state.device.start();
        }
        tokio::spawn(async move {
            let engine = catalogue_state.engine.clone();
            let Ok(registry) = eris_host::web::chat::provider_registry(&engine) else {
                return;
            };
            let missing: Vec<_> = registry
                .into_iter()
                .filter(|provider| {
                    engine
                        .provider_catalog(&provider.id)
                        .map(|catalog| catalog.is_empty())
                        .unwrap_or(true)
                })
                .collect();
            if missing.is_empty() {
                return;
            }
            let (probed, failed) = eris_host::web::routes::probe_registry(&engine, &missing).await;
            tracing::info!(
                probed,
                failed = failed.len(),
                "startup: published provider catalogues"
            );
        });
        let ticker = tokio::spawn(eris_host::scheduler::tick(scheduler_state));
        // The note-enrichment worker, on the same runtime and the same shape: one
        // background task, aborted with the listeners. It claims at most one run
        // per tick, so a queued enrichment is a billed provider call the operator
        // asked for rather than a queue that drains itself.
        let notes_state = state.clone();
        let notes_worker = tokio::spawn(eris_host::notes_worker::run(notes_state));
        // The learning loop, on the same runtime and the same shape: one
        // background task, aborted with the listeners. It sweeps for finished
        // answers, reviews at most one per tick, and runs the curator when its
        // interval has elapsed.
        let learning_state = state.clone();
        let learning_worker = tokio::spawn(eris_host::learning_worker::run(learning_state));
        let served = admin::serve_listeners(state, listeners).await;
        learning_worker.abort();
        notes_worker.abort();
        ticker.abort();
        served
    });

    // The listeners have stopped, by a signal or by a failure. What is left is
    // the runtime, and dropping a multi-thread runtime blocks until every
    // `spawn_blocking` task has finished. A device turn is one blocking task for
    // its whole provider loop, including the thirty-second command tool and the
    // provider's own streaming, so an implicit drop held the process open until
    // systemd's `TimeoutStopSec` expired and SIGKILLed it. Bound the wait
    // explicitly: ordinary work (a row being written) settles in milliseconds,
    // and a turn the operator interrupted by stopping the daemon is abandoned.
    // The next start settles its orphaned transcript row; a killed process left
    // the same row for the same recovery.
    runtime.shutdown_timeout(SHUTDOWN_GRACE);

    match outcome {
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

/// `eris roster` - the standing roster, one line per agent.
///
/// Read-only in the same way `doctor` is: this reports and exits. A CLI that
/// could edit a row would be a second editor for the table the dashboard owns,
/// and two editors is how the two disagree.
fn run_roster(json: bool, state_dir: Option<PathBuf>) -> ExitCode {
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris roster: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris roster: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let rows = match eris_host::roster::list(&db) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("eris roster: the roster could not be read: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    if json {
        let agents: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "name": row.name,
                    "role": row.role,
                    "tool_policy": row.tool_policy.as_str(),
                    "model_role": row.model_role,
                    "memory_scope": row.memory_scope,
                    "enabled": row.enabled,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "status": "ok", "roster": agents })
        );
    } else if rows.is_empty() {
        println!("no standing agents");
    } else {
        for row in &rows {
            let state = if row.enabled { "enabled" } else { "disabled" };
            println!("{} - {} ({state})", row.name, row.role);
        }
    }
    ExitCode::SUCCESS
}

/// `eris budget` - the same report the `budget` tool hands a turn.
///
/// It reads through `budget::report`, which reads through `metrics::summary_in`,
/// which is the function the settings panel and the phone footer call. A CLI that
/// computed its own numbers would be a fourth place for them to disagree.
fn run_budget(
    window: &str,
    conversation: Option<&str>,
    json: bool,
    state_dir: Option<PathBuf>,
) -> ExitCode {
    let Some(window) = eris_host::metrics::Window::parse(window) else {
        eprintln!("eris budget: '{window}' is not a window. Use 24h, 7d or all.");
        return ExitCode::from(EXIT_FAILURE);
    };
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris budget: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris budget: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let stored = eris_host::web::store::settings_get(&db, eris_host::budget::SETTING_SOFT_CAP)
        .ok()
        .flatten();
    let cap = eris_host::budget::soft_cap(stored.as_deref());
    let report = match eris_host::budget::report(&db, conversation, cap, window) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("eris budget: the window could not be read: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "report": eris_host::budget::to_json(&report),
            })
        );
    } else {
        println!(
            "{} - {} turns, {} tokens of {}, {} left",
            report.window,
            report.turns,
            report.total_tokens,
            report.soft_cap_tokens,
            report.remaining_tokens
        );
        println!("{}", report.verdict());
    }
    ExitCode::SUCCESS
}

/// `eris trigger list` - the webhook triggers, one line each.
///
/// Read-only, and deliberately not an editor: minting a token from a shell would
/// print a credential into a terminal history, which is the one thing the token
/// rule exists to prevent. The dashboard mints; this reports.
fn run_trigger(action: &str, json: bool, state_dir: Option<PathBuf>) -> ExitCode {
    if action != "list" {
        eprintln!(
            "eris trigger: '{action}' is not a command. The only one is 'list', because \
             minting a token here would print a credential into a shell history."
        );
        return ExitCode::from(EXIT_FAILURE);
    }
    let paths = match resolve_paths(state_dir) {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("eris trigger: {message}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            eprintln!(
                "eris trigger: cannot open {}: {error}",
                paths.db_file.display()
            );
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let rows = match eris_host::trigger::list(&db) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("eris trigger: {}", error.reason());
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    if json {
        let triggers: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "name": row.name,
                    "enabled": row.enabled,
                    "token_prefix": row.token_prefix,
                    "fire_count": row.fire_count,
                    "last_fired_at": row.last_fired_at,
                    "max_per_hour": row.max_per_hour,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "status": "ok", "triggers": triggers })
        );
    } else if rows.is_empty() {
        println!("no triggers");
    } else {
        for row in &rows {
            let state = if row.enabled { "enabled" } else { "disabled" };
            let last = row.last_fired_at.as_deref().unwrap_or("never");
            println!(
                "{} - {} ({state}), {} fires, last {last}",
                row.name, row.token_prefix, row.fire_count
            );
        }
    }
    ExitCode::SUCCESS
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The runtime shape `run_serve` builds, with one blocking task on it. That
    /// task is the stand-in for a turn: a device or browser turn is one
    /// `spawn_blocking` call for its whole provider loop.
    fn runtime_with_blocking_sleep(seconds: u64) -> tokio::runtime::Runtime {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a multi-thread runtime");
        runtime.spawn_blocking(move || std::thread::sleep(Duration::from_secs(seconds)));
        runtime
    }

    /// The shape the daemon had: the runtime falls out of scope, and its drop
    /// waits for the blocking task however long it runs. Ignored because it
    /// deliberately takes the whole sleep; run it with `--ignored`.
    #[test]
    #[ignore = "demonstrates the unbounded wait, so it takes as long as the task"]
    fn dropping_the_runtime_waits_for_a_blocking_task() {
        let runtime = runtime_with_blocking_sleep(5);
        let started = Instant::now();
        drop(runtime);
        let elapsed = started.elapsed();
        eprintln!("implicit drop returned after {elapsed:?} (five-second blocking task)");
        assert!(
            elapsed >= Duration::from_secs(5),
            "the implicit drop returned after {elapsed:?}; it was expected to wait out the task"
        );
    }

    /// The shape this binary now uses: the wait is bounded, so a turn that is
    /// still streaming cannot hold the process open.
    #[test]
    fn shutdown_timeout_does_not_wait_for_a_blocking_task() {
        let runtime = runtime_with_blocking_sleep(10);
        let started = Instant::now();
        runtime.shutdown_timeout(Duration::from_millis(100));
        let elapsed = started.elapsed();
        eprintln!("shutdown_timeout returned after {elapsed:?} (ten-second blocking task)");
        assert!(
            elapsed < Duration::from_secs(1),
            "shutdown_timeout waited {elapsed:?} for a ten-second task"
        );
    }

    /// The bound `run_serve` uses, against a task far longer than it. This is the
    /// real constant, so the sleep must outlast it for the assertion to mean
    /// anything.
    #[test]
    #[ignore = "takes SHUTDOWN_GRACE seconds; run with --ignored"]
    fn the_daemon_grace_returns_seconds_before_the_task_ends() {
        let runtime = runtime_with_blocking_sleep(30);
        let started = Instant::now();
        runtime.shutdown_timeout(SHUTDOWN_GRACE);
        let elapsed = started.elapsed();
        eprintln!(
            "shutdown_timeout({SHUTDOWN_GRACE:?}) returned after {elapsed:?} (thirty-second blocking task)"
        );
        assert!(
            elapsed >= SHUTDOWN_GRACE && elapsed < SHUTDOWN_GRACE + Duration::from_secs(2),
            "shutdown_timeout returned after {elapsed:?}, not within {SHUTDOWN_GRACE:?}"
        );
    }
}
