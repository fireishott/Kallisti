//! The coding harnesses themselves: present, signed in, and installable.
//!
//! A harness is not a provider model. The daemon's catalogue is a list of ids it
//! can call over its own provider connection; a harness is a separate program
//! with its own sign-in, its own model names and its own idea of a default. So
//! handing work to one is only half the seam: an operator also has to know
//! whether the program exists on this host, which account the Claude one is
//! signed in with, and what to run when the answer is "no program at all".
//!
//! Four rules shape this module.
//!
//! - **A search, not a guess.** `installed` is a binary found in a directory
//!   `PATH` names, and `version` is what that binary printed when asked. A
//!   credentials file beside a missing binary is not an installation.
//! - **A credential is described, never read back.** The OAuth sign-in is
//!   checked through three sources in order, and the answer carries only where
//!   it came from, whether it is still valid, and the account email. The token
//!   itself never leaves this module: not into a body, not into a log line.
//! - **One fixed model list per harness.** A coding tool accepts a handful of
//!   names, not the daemon's whole catalogue, so each list is written down here
//!   and [`supports_model`] is the one answer to "may this be stored".
//! - **An install is a harness run.** Starting one registers a record in
//!   [`crate::harness_runs`] before its process exists, streams into the same
//!   tail a delegated task does, and is bounded by the same watchdog. There is no
//!   second monitor, no second deadline and no second kill.
//!
//! Nothing here answers a request: it reports, it decides from a report alone in
//! [`install_plan`], and it starts one documented command in [`start_install`].
//! The status code belongs to the route, which is also the only caller that
//! decides anything.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::harness_runs::{self, RunStatus};

/// The harness id the Claude tool is stored under.
pub const TOOL_CLAUDE_CODE: &str = "claude-code";

/// The harness id the DeepSeek Harness CLI is stored under.
pub const TOOL_DSH: &str = "dsh";

/// The harness id the Antigravity tool is stored under.
pub const TOOL_ANTIGRAVITY: &str = "antigravity";

/// Every harness this build can drive, in the order a picker lists them.
pub const IDS: [&str; 3] = [TOOL_DSH, TOOL_CLAUDE_CODE, TOOL_ANTIGRAVITY];

/// The command that installs the Claude tool.
pub const CLAUDE_CODE_INSTALL: &str = "npm install -g @anthropic-ai/claude-code";

/// The command that installs the DeepSeek Harness CLI.
pub const DSH_INSTALL: &str = "npm install -g @deepseek-ai/dsh";

/// The command that installs Antigravity on macOS.
///
/// The same installer script as everywhere else: the npm package of that name
/// is an empty placeholder, so installing it succeeds and leaves nothing to run.
pub const ANTIGRAVITY_INSTALL_MACOS: &str =
    "curl -fsSL https://antigravity.google/cli/install.sh | bash";

/// The command that installs Antigravity everywhere else.
pub const ANTIGRAVITY_INSTALL: &str = "curl -fsSL https://antigravity.google/cli/install.sh | bash";

/// How long a binary may take to answer `--version`.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the macOS keychain may take to answer.
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The keychain item the Claude tool stores its sign-in in.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// The deadline an install is given in the run registry.
pub const INSTALL_TIMEOUT_SECS: u64 = 600;

/// The package manager that installs two of the three.
const NPM: &str = "npm";

/// What fetches Antigravity's installer script.
const CURL: &str = "curl";

/// The macOS binary that reads the keychain.
const KEYCHAIN_BINARY: &str = "security";

/// How often a child is polled while it is being given its deadline.
const POLL: Duration = Duration::from_millis(10);

/// How long a finished child's pipe may take to deliver bytes it already has.
const PIPE_GRACE: Duration = Duration::from_millis(500);

/// Where a coding harness's OAuth sign-in came from.
pub const SOURCE_ENVIRONMENT: &str = "environment";
/// A token saved through this daemon's own route.
pub const SOURCE_DAEMON: &str = "saved here";
/// See [`SOURCE_ENVIRONMENT`].
pub const SOURCE_CREDENTIALS_FILE: &str = "credentials file";
/// See [`SOURCE_ENVIRONMENT`].
pub const SOURCE_KEYCHAIN: &str = "keychain";
/// No source held a credential at all.
pub const SOURCE_NONE: &str = "none";

/// The only `auth.kind` this module reports: an OAuth sign-in.
pub const OAUTH: &str = "oauth";

/// The platform detection runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux.
    Linux,
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
    /// Anything else.
    Other,
}

impl Platform {
    /// The platform this process was built for.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }

    /// Whether this host can install a harness at all.
    ///
    /// The two packaged platforms, and nothing else: a hint is not a promise, and
    /// claiming Windows can run the same two package managers would be one.
    #[must_use]
    pub fn installs(self) -> bool {
        matches!(self, Self::Linux | Self::MacOs)
    }
}

impl Serialize for Platform {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One model a coding harness accepts.
///
/// The ids are the tool's own spellings, not the daemon's catalogue ids: a
/// harness resolves `sonnet` to whatever its current Sonnet is, and handing it
/// `provider/model` would either fail or mean something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Model {
    /// The id passed through as `--model`.
    pub id: &'static str,
    /// What a picker shows.
    pub label: &'static str,
}

/// The models the Claude tool accepts.
pub const CLAUDE_CODE_MODELS: [Model; 4] = [
    Model {
        id: "sonnet",
        label: "Sonnet",
    },
    Model {
        id: "opus",
        label: "Opus",
    },
    Model {
        id: "haiku",
        label: "Haiku",
    },
    Model {
        id: "default",
        label: "its own default",
    },
];

/// The models Antigravity accepts.
pub const ANTIGRAVITY_MODELS: [Model; 3] = [
    Model {
        id: "gemini-3-pro",
        label: "Gemini 3 Pro",
    },
    Model {
        id: "gemini-3-flash",
        label: "Gemini 3 Flash",
    },
    Model {
        id: "default",
        label: "its own default",
    },
];

/// The models the DeepSeek Harness CLI accepts.
///
/// One: the tool picks its own model, and the daemon has nothing to say about it.
pub const DSH_MODELS: [Model; 1] = [Model {
    id: "default",
    label: "its own default",
}];

/// One harness's fixed model list, or `None` for a name this build does not have.
#[must_use]
pub fn models(harness: &str) -> Option<&'static [Model]> {
    match harness.trim() {
        TOOL_CLAUDE_CODE => Some(&CLAUDE_CODE_MODELS),
        TOOL_ANTIGRAVITY => Some(&ANTIGRAVITY_MODELS),
        TOOL_DSH => Some(&DSH_MODELS),
        _ => None,
    }
}

/// The model ids one harness accepts, in the order a picker lists them.
#[must_use]
pub fn allowed_models(harness: &str) -> Vec<&'static str> {
    models(harness)
        .map(|models| models.iter().map(|model| model.id).collect())
        .unwrap_or_default()
}

/// Whether `harness` accepts `model`.
///
/// The empty string is always accepted and means "the tool's own default": that
/// is the shipped state, and refusing it would mean an operator could not clear a
/// row once set. A name outside the list is refused, which is the whole point:
/// a coding tool given a provider catalogue id fails at run time, long after the
/// operator stopped looking.
#[must_use]
pub fn supports_model(harness: &str, model: &str) -> bool {
    let model = model.trim();
    if model.is_empty() {
        return true;
    }
    models(harness).is_some_and(|models| models.iter().any(|accepted| accepted.id == model))
}

/// What this module knows about the Claude tool's sign-in.
///
/// The token is deliberately absent. `source` names where the credential was
/// found, `verified` says whether it is still valid, and `account` is the email
/// the credential carried when it carried one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Auth {
    /// Always [`OAUTH`].
    pub kind: &'static str,
    /// Whether a credential was found and has not expired.
    pub verified: bool,
    /// The account email, when the credential names one.
    pub account: Option<String>,
    /// `environment`, `credentials file`, `keychain`, or `none`.
    pub source: &'static str,
}

/// One harness, as `GET /v1/harness/tools` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tool {
    /// The stored harness name.
    pub id: &'static str,
    /// What a screen shows.
    pub name: &'static str,
    /// The binary the built-in command starts.
    pub binary: &'static str,
    /// Whether that binary was found in a directory `PATH` names.
    pub installed: bool,
    /// The first line of `--version`, or `None` when it did not answer.
    pub version: Option<String>,
    /// The absolute path the binary was found at.
    pub path: Option<String>,
    /// Whether this host has the platform and the package manager to install it.
    pub installable: bool,
    /// The exact command that would install it.
    pub install_hint: String,
    /// The sign-in, for the harness that has one.
    ///
    /// Reported whether or not the binary is installed: the credential is a fact
    /// about the account, and an operator who has just installed the tool wants to
    /// know the sign-in it will pick up. `None` for the harnesses that have no
    /// sign-in of their own.
    pub auth: Option<Auth>,
    /// The models the tool accepts.
    pub models: &'static [Model],
}

/// Every harness plus the platform the search ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolsReport {
    /// The platform the search and the install decision were made for.
    pub platform: Platform,
    /// One row per harness, in [`IDS`] order.
    pub tools: Vec<Tool>,
}

impl ToolsReport {
    /// The body `GET /v1/harness/tools` answers, built in one place.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "platform": self.platform,
            "tools": self.tools,
        })
    }
}

/// One of the harnesses this build can drive.
struct ToolDef {
    /// The stored harness name.
    id: &'static str,
    /// What a screen shows.
    name: &'static str,
    /// The binary names the tool is installed as.
    binary: &'static str,
    /// The models the tool accepts.
    models: &'static [Model],
}

/// The three harnesses, in the order the routing picker lists them.
const TOOLS: [ToolDef; 3] = [
    ToolDef {
        id: TOOL_DSH,
        name: "DeepSeek Harness",
        binary: "dsh",
        models: &DSH_MODELS,
    },
    ToolDef {
        id: TOOL_CLAUDE_CODE,
        name: "Claude Code",
        binary: "claude",
        models: &CLAUDE_CODE_MODELS,
    },
    ToolDef {
        id: TOOL_ANTIGRAVITY,
        name: "Antigravity",
        binary: "agy",
        models: &ANTIGRAVITY_MODELS,
    },
];

/// Where detection looks: the process environment, or a fixture.
///
/// Production reads [`Env::process`] and nothing else, through [`detect`]. The
/// struct is public because the alternative - rewriting a live process's `PATH` -
/// is not something a test can do safely, and a detection function that could not
/// be pointed at a directory of stubs could only be tested against whatever this
/// host happens to have installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    /// The directories `PATH` names, in its own order.
    pub dirs: Vec<PathBuf>,
    /// The home directory, where the Claude credentials file lives.
    pub home: Option<PathBuf>,
    /// `CLAUDE_CODE_OAUTH_TOKEN`, when it is set.
    pub oauth_token: Option<String>,
    /// A token saved through the daemon's own route. It outranks the
    /// environment because it is the one a person deliberately stored.
    pub stored_token: Option<String>,
    /// The platform to decide the search and the install hint for.
    pub platform: Platform,
}

impl Env {
    /// The process's own environment.
    #[must_use]
    pub fn process() -> Self {
        Self {
            dirs: Self::search_dirs(),
            home: std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
            oauth_token: std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
                .ok()
                .filter(|token| !token.trim().is_empty()),
            stored_token: None,
            platform: Platform::current(),
        }
    }

    /// The daemon's `PATH` plus the places a user's tools actually live.
    ///
    /// A systemd user service starts with a bare `PATH`, so `dsh` in
    /// `~/.local/bin` and a Homebrew binary in `/opt/homebrew/bin` read as
    /// missing on a host that has both. Guessing these few is the difference
    /// between an honest report and a false one.
    fn search_dirs() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from);
        let mut extra: Vec<PathBuf> = vec![
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/bin"),
        ];
        if let Some(home) = home {
            extra.push(home.join(".local/bin"));
            extra.push(home.join(".npm-global/bin"));
            extra.push(home.join(".cargo/bin"));
            extra.push(home.join(".dsh/bin"));
            extra.push(home.join(".claude/local"));
        }
        for dir in extra {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }
}

/// A sign-in in flight: the Claude CLI the daemon opened, and the URL it printed.
///
/// The CLI's own flow is the flow: it prints an authorization URL, a person signs
/// in, and it takes the code back on standard input. Driving it from the daemon
/// means the phone shows a button and a code field instead of a terminal command.
pub struct LoginSession {
    child: std::process::Child,
    stdout: std::sync::mpsc::Receiver<String>,
    stderr: std::sync::mpsc::Receiver<String>,
    pub id: String,
    pub url: String,
    started: std::time::Instant,
}

/// The harnesses with an install running right now.
///
/// Keyed by harness id. The vendor installers are not reentrant - Antigravity
/// stages its download in one fixed path - so two runs of one installer is a
/// checksum failure that reads like a corrupt download.
static INSTALLS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn installs() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    INSTALLS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Give back the install claim for one harness.
fn release_install(id: &str) {
    if let Ok(mut busy) = installs().lock() {
        busy.remove(id);
    }
}

/// Every sign-in the daemon has open, newest last. Bounded by the sweep below.
static LOGINS: std::sync::OnceLock<std::sync::Mutex<Vec<LoginSession>>> =
    std::sync::OnceLock::new();

fn logins() -> &'static std::sync::Mutex<Vec<LoginSession>> {
    LOGINS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Remove the terminal control sequences a pseudo-terminal interleaves.
///
/// `claude setup-token` only renders on a terminal, so its stream carries CSI
/// and OSC escapes and hyperlink wrappers around the two things worth lifting
/// out of it. Stripping them here is what lets the scan below be a substring
/// search: an OSC wrapper ends in a printable character, which makes the URL the
/// tail of a "word" to anything that splits on whitespace first.
fn strip_terminal_escapes(line: &str) -> String {
    strip_escapes(line, false)
}

/// [`strip_terminal_escapes`], with a cursor move across a row read as a space.
///
/// An Ink screen draws words by moving the cursor, not by printing the space
/// between them, so a sentence read without that gap arrives as one long word.
fn strip_escapes(line: &str, gaps: bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            if character != '\u{7}' && character != '\r' {
                out.push(character);
            }
            continue;
        }
        match chars.next() {
            // CSI: ESC [ parameters, then one byte in 0x40..=0x7e.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        if gaps && (next == 'G' || next == 'C') {
                            out.push(' ');
                        }
                        break;
                    }
                }
            }
            // OSC: ESC ] text, ended by BEL or by ESC \.
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Two-character escapes, and a truncated one at the end of a chunk.
            Some(_) | None => {}
        }
    }
    out
}

/// The first run of text after `prefix`, cut at whitespace or a control byte.
///
/// The URL and the token are the only two things the sign-in says that matter,
/// and on a terminal both arrive glued to prompt text and escape sequences. This
/// is the one place that knows how they end.
fn first_field_with(line: &str, prefix: &str) -> Option<String> {
    let clean = strip_terminal_escapes(line);
    let start = clean.find(prefix)?;
    let tail = &clean[start..];
    let mut end = tail.len();
    for (index, character) in tail.char_indices() {
        let stops = character.is_whitespace()
            || character.is_control()
            || character == ')'
            || character == ','
            // A hyperlink wrapper prints the value twice around a BOM; stop at
            // the second copy rather than returning both run together.
            || character == '\u{feff}'
            || (index > 0 && tail[index..].starts_with(prefix));
        if stops {
            end = index;
            break;
        }
    }
    Some(tail[..end].trim().to_owned())
}

/// A command that gives its child a terminal.
///
/// The CLI writes nothing at all to a pipe: spawned plainly from a service, its
/// sign-in reads as "did not print a sign-in link" on a host where the same
/// command works by hand. `script` is the smallest thing on this box that
/// supplies a pty, and its `-c` form keeps the child on our stdin and stdout, so
/// the code still goes in through the pipe the caller already writes to. The
/// width is set first because the token is longer than a default 80-column
/// terminal and a wrapped token would be read back in halves.
fn terminal_command(claude: &Path, dirs: &[PathBuf]) -> Command {
    let Some(script) = find_on_path(dirs, "script") else {
        let mut plain = Command::new(claude);
        plain.arg("setup-token");
        return plain;
    };
    let path = claude.display().to_string().replace('\'', "'\\''");
    let mut command = Command::new(script);
    command
        .arg("-q")
        .arg("-e")
        .arg("-c")
        .arg(format!(
            "stty cols 500 rows 50 2>/dev/null; '{path}' setup-token"
        ))
        .arg("/dev/null");
    command
}

/// Start `claude setup-token` and read the URL it prints.
///
/// # Errors
///
/// A string naming why no sign-in could be opened: the binary is missing, it did
/// not print a URL within ten seconds, or the registry could not be locked.
pub fn start_login(env: &Env) -> Result<(String, String), String> {
    let Some(claude) = find_on_path(&env.dirs, "claude") else {
        return Err("Claude Code is not installed on this host".to_owned());
    };
    let mut child = terminal_command(&claude, &env.dirs)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("Claude Code could not be started: {error}"))?;
    let stdout = child.stdout.take().ok_or("no output pipe")?;
    let rx = chunk_reader(stdout);
    // stderr is drained too: the CLI writes its own failures there, and a pipe
    // nobody reads fills and stops the child mid-sign-in.
    let stderr_rx = child.stderr.take().map(chunk_reader);
    // The screen is drawn in pieces, so the link is read from everything seen so
    // far, and only once a line break follows it: a link cut at a chunk edge is
    // a link that opens the wrong page.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut seen = String::new();
    let mut url: Option<String> = None;
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(chunk) => {
                seen.push_str(&chunk);
                if let Some(found) = finished_field(&seen, "https://") {
                    url = Some(found);
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let Some(url) = url else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Claude Code did not print a sign-in link".to_owned());
    };
    let id = uuid::Uuid::now_v7().to_string();
    // A sign-in nobody finishes is not worth keeping.
    if let Ok(mut open) = logins().lock() {
        open.retain(|session| session.started.elapsed() < std::time::Duration::from_secs(600));
    }
    logins()
        .lock()
        .map_err(|_| "the sign-in list is unavailable".to_owned())?
        .push(LoginSession {
            child,
            stdout: rx,
            stderr: stderr_rx.unwrap_or_else(|| std::sync::mpsc::channel().1),
            id: id.clone(),
            url: url.clone(),
            started: std::time::Instant::now(),
        });
    Ok((id, url))
}

/// Hand the code back to the CLI and take the token it prints.
///
/// # Errors
///
/// A string naming why the sign-in did not complete.
pub fn complete_login(id: &str, code: &str) -> Result<String, String> {
    use std::io::Write;
    // What a person actually pastes is whatever their browser showed them. When
    // that is the callback URL rather than the short code, the CLI answers
    // "Invalid code" and the sign-in fails for a reason the operator cannot see.
    let code = code_from_pasted(code);
    // The sign-in is only held for the length of this wait, so a second attempt
    // after a bad code starts from a fresh session rather than blocking here.
    let mut session = {
        let mut open = logins()
            .lock()
            .map_err(|_| "the sign-in list is unavailable".to_owned())?;
        let Some(index) = open.iter().position(|session| session.id == id) else {
            return Err("no sign-in with that id is open; start one again".to_owned());
        };
        open.remove(index)
    };

    // The prompt is drawn after the link, and a code typed before it exists is a
    // code the CLI can drop: the write lands in the pty and the line discipline
    // discards it before the reader is attached. So the screen is read until the
    // prompt is up, and only then is the code sent. A prompt that never arrives is
    // recorded rather than guessed at, because the failure has to name which of the
    // two happened: no prompt, or a refused code.
    let mut seen = String::new();
    let prompt_deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    let mut prompt_seen = false;
    while std::time::Instant::now() < prompt_deadline {
        match session
            .stdout
            .recv_timeout(std::time::Duration::from_millis(200))
        {
            Ok(chunk) => {
                seen.push_str(&chunk);
                if strip_escapes(&seen, true).to_lowercase().contains("paste") {
                    prompt_seen = true;
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // A carriage return, not a newline: the prompt reads the terminal in raw
    // mode, where Enter is CR. A bare newline is echoed and then ignored, which left
    // the app spinning while the CLI waited for a key it never saw.
    let stdin = session
        .child
        .stdin
        .as_mut()
        .ok_or("the sign-in has no input pipe")?;
    stdin
        .write_all(format!("{}\r", code.trim()).as_bytes())
        .map_err(|error| format!("the code could not be sent: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("the code could not be sent: {error}"))?;

    // Bounded, because the phone is waiting on this call. A rejection arrives in
    // seconds; anything past half a minute is the CLI waiting for another code,
    // and saying so beats an endless spinner.
    //
    // The CLI redraws its screen in place, so what it says arrives as cursor
    // moves and carriage returns with no line break at all. Everything is read
    // as raw chunks and scanned as one growing text.
    // How long a token exchange may take.
    //
    // Was 30 seconds, which is shorter than the exchange itself can be: a
    // well-formed code sends the CLI to console.anthropic.com, and when that
    // endpoint is slow or rate-limiting the CLI prints nothing while it waits. The
    // daemon then gave up, reported "no token and no error", and the operator
    // pressed the button again - which is the one action that keeps a rate limit in
    // place. Two minutes is longer than any real exchange needs and still bounded,
    // because the phone is waiting on this call.
    let deadline_secs: u64 = 120;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
    let mut token: Option<String> = None;
    let mut complaint: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let mut saw_chunk = false;
        if let Ok(chunk) = session
            .stdout
            .recv_timeout(std::time::Duration::from_millis(200))
        {
            saw_chunk = true;
            seen.push_str(&chunk);
        }
        if let Ok(chunk) = session
            .stderr
            .recv_timeout(std::time::Duration::from_millis(50))
        {
            saw_chunk = true;
            seen.push_str(&chunk);
        }
        if saw_chunk {
            token = first_field_with(&seen, "sk-ant-");
            if token.is_some() {
                break;
            }
            complaint = login_complaint(&seen);
            if complaint.is_some() {
                // A complaint means the CLI is back at its prompt. Give it a
                // moment to print a token it may have written in the same
                // redraw, then answer.
                if let Ok(chunk) = session
                    .stdout
                    .recv_timeout(std::time::Duration::from_millis(300))
                {
                    seen.push_str(&chunk);
                    token = first_field_with(&seen, "sk-ant-");
                }
                break;
            }
        } else if session.child.try_wait().ok().flatten().is_some() {
            break;
        }
    }

    // Whatever the outcome, this sign-in is over. A code works once and only for
    // the link it came from, and a child left at its prompt is a process nobody
    // will ever answer.
    let _ = session.child.kill();
    let _ = session.child.wait();

    match token {
        Some(found) => Ok(found),
        None => Err(match complaint {
            Some(said) => said,
            // No complaint and no token is not one failure but several, and the
            // operator can only act on the one that happened. The CLI's own last
            // words are carried through, because they are the difference between a
            // code that was refused, a subscription that is not entitled to a
            // token, and a prompt that never appeared at all.
            None => {
                let exit = session
                    .child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|status| status.code());
                let drawn = last_words(&seen);
                // A well-formed code makes the CLI do a real network exchange, and
                // while that exchange hangs it prints nothing at all - no token, no
                // error, no redraw. The old sentence said "within 30 seconds", which
                // read as the daemon's own timeout and hid the fact that the CLI was
                // still waiting on Anthropic. Say what is actually true: the code
                // went in, the exchange did not come back.
                let waited = deadline_secs;
                let mut sentence = if prompt_seen {
                    format!(
                        "Claude Code accepted that code and then went quiet: no token and no error \
                         came back within {waited} seconds. That is the exchange against Anthropic \
                         not answering, not a bad code. Check this host can reach \
                         console.anthropic.com (a 429 there looks exactly like this), then try again"
                    )
                } else {
                    "Claude Code never drew its code prompt, so the code had nowhere to go. Start the sign-in again and watch for the prompt before pasting".to_owned()
                };
                if let Some(code) = exit {
                    sentence.push_str(&format!(" (it exited with status {code})"));
                }
                if !drawn.is_empty() {
                    sentence.push_str(&format!(". It last printed: {drawn}"));
                }
                sentence
            }
        }),
    }
}

/// The code inside whatever was pasted.
///
/// A `claude setup-token` code is not one bare token. What the browser shows is
/// the OAuth authorization code joined to the flow's `state` by a `#`, and the
/// CLI splits on that separator before it exchanges the pair. This function used
/// to cut the pasted text at `#`, which threw the state away: the CLI then had a
/// code with no state and answered a 400, which the phone rendered as "Claude
/// refused that code". It was never refused - it was sent incomplete.
///
/// Three shapes arrive from a phone and all three have to produce the same pair:
///
/// - `code#state`, copied straight off the callback page, passed through whole;
/// - the whole callback URL, where both halves are query parameters;
/// - a bare code, for a deployment whose page shows only that.
fn code_from_pasted(pasted: &str) -> String {
    let trimmed = pasted.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // A callback URL carries the code as a parameter. When it does, both halves
    // are read and rejoined in the CLI's own format, so a password manager that
    // pastes the URL and a person who copies the shown code take one path.
    if trimmed.contains("://") || trimmed.contains("code=") {
        let code = query_value(trimmed, "code");
        let state = query_value(trimmed, "state");
        if let Some(code) = code.filter(|code| !code.is_empty()) {
            return match state.filter(|state| !state.is_empty()) {
                Some(state) => format!("{code}#{state}"),
                None => code,
            };
        }
        return trimmed.to_owned();
    }
    // A code already in `code#state` form is kept whole. The state is a required
    // half of the pair, not a comment to be trimmed off.
    if let Some((code, state)) = trimmed.split_once('#') {
        let code = code.trim();
        let state = state.trim();
        if !code.is_empty() && !state.is_empty() {
            return format!("{code}#{state}");
        }
        // A trailing `#` with nothing after it is a copy artefact, not a state.
        return code.to_owned();
    }
    trimmed.to_owned()
}

/// One query parameter out of a URL, percent-decoded.
///
/// Deliberately not a URL parser: this reads the two parameters it was written
/// for, from text that may be a URL, a fragment of one, or a line a person
/// pasted with the label still attached.
fn query_value(url: &str, name: &str) -> Option<String> {
    // Matched only at a `?` or `&` boundary, so `state` is not found inside
    // `code_challenge_method` or any other key that merely contains it.
    let mut offset = 0usize;
    while let Some(found) = url[offset..].find(name) {
        let at = offset + found;
        let before = if at == 0 {
            None
        } else {
            url[..at].chars().next_back()
        };
        let after = url[at + name.len()..].chars().next();
        let starts_field = matches!(before, None | Some('?') | Some('&'));
        if starts_field && after == Some('=') {
            let value_start = at + name.len() + 1;
            let tail = &url[value_start..];
            let end = tail
                .find(['&', '#', '\n', '\r', ' ', '"', '\''])
                .unwrap_or(tail.len());
            let raw = tail[..end].trim();
            return Some(percent_decode(raw));
        }
        // Not this occurrence; look past it.
        offset = at + name.len();
    }
    None
}

/// Decode the percent escapes a URL parameter carries.
///
/// The code and state are base64url in practice, so the common case has nothing
/// to decode and this returns the input unchanged.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &raw[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The last thing a redrawn screen said, escapes gone and on one line.
///
/// Bounded to the tail because an Ink screen redraws the whole frame: the useful
/// words are the last ones drawn, and everything before them is the same header
/// printed again.
fn last_words(seen: &str) -> String {
    let clean = strip_escapes(seen, true);
    let joined = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = joined
        .chars()
        .rev()
        .take(240)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    clipped
}

/// Read a pipe as raw chunks, which is the only way a redrawn screen is seen.
///
/// A line reader waits for a newline the CLI never sends after a rejection, so
/// the rejection is never read.
fn chunk_reader(pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut buffer = [0u8; 4096];
        // A multi-byte character can be cut at a chunk edge; carry the tail.
        let mut carry: Vec<u8> = Vec::new();
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    carry.extend_from_slice(&buffer[..count]);
                    let valid = match std::str::from_utf8(&carry) {
                        Ok(_) => carry.len(),
                        Err(error) => error.valid_up_to(),
                    };
                    if valid == 0 {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
                    carry.drain(..valid);
                    if tx.send(text).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// The first field after `prefix`, but only once the screen has moved past it.
///
/// A value still being drawn is cut at the end of what has arrived; waiting for
/// a line break after it is what says it is whole.
fn finished_field(seen: &str, prefix: &str) -> Option<String> {
    let clean = strip_terminal_escapes(seen);
    let start = clean.find(prefix)?;
    if !clean[start..].contains('\n') {
        return None;
    }
    first_field_with(seen, prefix)
}

/// The CLI's own words when it rejects a code, so the phone can say them.
///
/// Read from everything the CLI has drawn. Words are matched with the spacing
/// removed, because a redraw leaves gaps and skipped letters inside them.
fn login_complaint(seen: &str) -> Option<String> {
    let spaced = strip_escapes(seen, true);
    // Whitespace AND punctuation are dropped before matching.
    //
    // A redraw glues words together with cursor moves, so "rate limit" can arrive
    // as "ratelimit". The CLI also prints structured errors, where the same fact
    // is spelled `rate_limit_error` - and the underscore is enough to break a plain
    // whitespace squeeze, which is why a 429 in JSON form went unrecognised.
    let squeezed: String = spaced
        .chars()
        .filter(|character| {
            !character.is_whitespace() && !matches!(character, '_' | '-' | '"' | '\'' | '.' | ':')
        })
        .collect::<String>()
        .to_lowercase();
    // The CLI's own format check, which runs before any network call. Its text
    // is "Invalid code. Please make sure the full code was copied", and it means
    // the value never left the machine - the pasted text was not in the
    // `code#state` shape the CLI expects. That is a different failure from a code
    // the server refused, and it has a different fix, so it is reported first and
    // says which one happened.
    if squeezed.contains("invalidcode") && !squeezed.contains("statuscode") {
        return Some(
            "That does not look like a complete code. Claude Code expects the whole value from the page, including the part after the #. Close this, tap Sign in with Claude again, and copy the entire code"
                .to_owned(),
        );
    }
    // A rate limit is not a refused code and must not read as one.
    //
    // The OAuth token endpoint answers `429 rate_limit_error` while it is holding
    // this host off, and the CLI surfaces that as "Request failed with status code
    // 429". Reporting it as "Claude refused that code" sends the operator to fetch
    // a fresh code for a server that is refusing everyone, and the fresh code is
    // refused in the same way.
    if squeezed.contains("statuscode429") || squeezed.contains("ratelimit") {
        return Some(
            "Claude is rate-limiting this host, not refusing your code (429). Nothing about \
             the code is wrong. Wait a few minutes, then start the sign-in again - tapping \
             it repeatedly keeps the limit in place"
                .to_owned(),
        );
    }
    let refused = [
        "statuscode400",
        "statuscode401",
        "invalid_grant",
        "invalidgrant",
    ];
    if refused.iter().any(|needle| squeezed.contains(needle)) {
        return Some(
            "Claude refused that code (400). A code works once, and only for the link it came from: close this, tap Sign in with Claude again, and paste the code from the new page"
                .to_owned(),
        );
    }
    if squeezed.contains("expired") {
        return Some(
            "That code has expired. Close this, tap Sign in with Claude again, and paste the code from the new page"
                .to_owned(),
        );
    }
    let other = ["oautherror", "requestfailed", "denied", "pressentertoretry"];
    if other.iter().any(|needle| squeezed.contains(needle)) {
        let joined = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
        let tail: String = joined
            .chars()
            .rev()
            .take(160)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Some(format!("Claude Code said: {tail}"));
    }
    None
}

/// Kill every sign-in the daemon still has open.
///
/// `claude setup-token` waits on a terminal that will never answer once the
/// daemon is stopping, and a live child holds the service's cgroup open.
pub fn terminate_open_logins() {
    let Ok(mut open) = logins().lock() else {
        return;
    };
    for session in open.iter_mut() {
        let _ = session.child.kill();
    }
    open.clear();
}

/// The environment detection reads, fixture-aware under test.
#[must_use]
pub fn detect_env() -> Env {
    search_env()
}

/// Detect every harness on this host.
#[must_use]
pub fn detect() -> ToolsReport {
    detect_in(&search_env())
}

/// Detect every harness against an explicit environment.
#[must_use]
pub fn detect_in(env: &Env) -> ToolsReport {
    ToolsReport {
        platform: env.platform,
        tools: TOOLS.iter().map(|tool| inspect(tool, env)).collect(),
    }
}

/// The environment detection uses: the process's own, unless a test substituted
/// a fixture for the whole module.
fn search_env() -> Env {
    #[cfg(test)]
    if let Some(fixture) = testing::current() {
        return fixture;
    }
    Env::process()
}

/// Everything one tool is, on one host.
fn inspect(def: &ToolDef, env: &Env) -> Tool {
    let path = find_on_path(&env.dirs, def.binary);
    let version = path.as_deref().and_then(version_of);
    let installable = env.platform.installs()
        && installer_for(def.id, env.platform)
            .is_some_and(|installer| find_on_path(&env.dirs, installer).is_some());
    let auth = if def.id == TOOL_CLAUDE_CODE {
        Some(claude_auth(env))
    } else {
        None
    };
    Tool {
        id: def.id,
        name: def.name,
        binary: def.binary,
        installed: path.is_some(),
        version,
        path: path.map(|path| path.display().to_string()),
        installable,
        install_hint: install_command(def.id, env.platform)
            .unwrap_or_default()
            .to_owned(),
        auth,
        models: def.models,
    }
}

/// What a request to install one harness resolved to, before anything ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPlan {
    /// Start this exact command.
    Start(String),
    /// No harness in this build carries that id.
    Unknown,
    /// The binary is already on the path; there is nothing to do.
    Installed,
    /// This host cannot install it, and the hint is the command that would.
    Unsupported(String),
}

/// Decide what installing `id` would mean, from a report and nothing else.
///
/// Pure on purpose: the route answers four different statuses from this answer,
/// and a test can hand it a report instead of arranging a host.
#[must_use]
pub fn install_plan(report: &ToolsReport, id: &str) -> InstallPlan {
    let id = id.trim();
    let Some(tool) = report.tools.iter().find(|tool| tool.id == id) else {
        return InstallPlan::Unknown;
    };
    if tool.installed {
        return InstallPlan::Installed;
    }
    if !tool.installable {
        return InstallPlan::Unsupported(tool.install_hint.clone());
    }
    InstallPlan::Start(tool.install_hint.clone())
}

/// The exact command that installs one harness.
///
/// One string per harness per platform, and the same string is both the hint in
/// the report and the command the install route starts: a hint that differs from
/// what would run is a lie an operator only discovers afterwards.
#[must_use]
pub fn install_command(id: &str, platform: Platform) -> Option<&'static str> {
    match (id.trim(), platform) {
        (TOOL_CLAUDE_CODE, _) => Some(CLAUDE_CODE_INSTALL),
        (TOOL_DSH, _) => Some(DSH_INSTALL),
        (TOOL_ANTIGRAVITY, Platform::MacOs) => Some(ANTIGRAVITY_INSTALL_MACOS),
        (TOOL_ANTIGRAVITY, _) => Some(ANTIGRAVITY_INSTALL),
        _ => None,
    }
}

/// The package manager one harness needs on one platform.
fn installer_for(id: &str, platform: Platform) -> Option<&'static str> {
    match (id.trim(), platform) {
        (TOOL_CLAUDE_CODE | TOOL_DSH, _) => Some(NPM),
        (TOOL_ANTIGRAVITY, _) => Some(CURL),
        _ => None,
    }
}

/// Start one install as a registered harness run, and answer the run.
///
/// The command must be one of the constants above, chosen by harness id through
/// [`install_command`]: this function is the mechanism, not a general shell, and
/// the route that calls it passes `install_hint` and nothing else. No user input
/// is ever part of the string.
///
/// The run exists before the process does, so a failure to start is a recorded
/// ending rather than a silence, and it carries the same 600 second deadline and
/// the same watchdog every delegated task has. The output is fed to the registry
/// by the only readers there are, so a monitor costs the install nothing.
///
/// # Errors
///
/// A sentence for the route when the shell could not be started at all.
pub fn start_install(id: &str, command: &str) -> Result<serde_json::Value, String> {
    // One install per harness, because the vendor script is not reentrant: it
    // downloads to a fixed staging path under `~/.cache/antigravity/staging`, so a
    // second run overwrites the file the first one is still hashing. What the
    // operator saw was two `antigravity` installs started one after the other, and
    // both failed with "the downloaded payload checksum does not match the
    // manifest" - a true statement about a file the other run was rewriting, and a
    // lie about the download. The claim is taken before the row exists so a second
    // press is answered immediately instead of racing.
    let mut busy = installs()
        .lock()
        .map_err(|_| "the install list is unavailable".to_owned())?;
    if !busy.insert(id.to_owned()) {
        return Err(format!(
            "an install of {id} is already running: the vendor script stages into one \
             fixed path, so a second copy would corrupt the first one's download. \
             Wait for the running install to finish"
        ));
    }
    drop(busy);
    let task = format!("install: {command}");
    let run_id = harness_runs::start(id, &task, None, None, INSTALL_TIMEOUT_SECS);
    let mut child = match spawn_install(command) {
        Ok(child) => child,
        Err(error) => {
            release_install(id);
            harness_runs::finish(&run_id, RunStatus::Failed, None);
            return Err(format!("cannot start `{command}`: {error}"));
        }
    };
    let pid = child.id();
    harness_runs::attach_pid(&run_id, pid);
    // The two readers are the only ones: the registry is fed from the same pipe
    // this module would otherwise have read, so nothing is drained twice.
    drain(child.stdout.take(), run_id.clone(), Stream::Stdout);
    drain(child.stderr.take(), run_id.clone(), Stream::Stderr);
    // The reaper records the ending. A watchdog timeout lands first when the
    // install runs too long, and the first ending is the true one.
    let reaped = run_id.clone();
    let owned = id.to_owned();
    std::thread::spawn(move || {
        let status = child.wait();
        let code = status.ok().and_then(|status| status.code());
        let ending = if code == Some(0) {
            RunStatus::Succeeded
        } else {
            RunStatus::Failed
        };
        harness_runs::finish(&reaped, ending, code);
        // The claim is released by the reaper, which is the only path that knows
        // the process is gone. A watchdog timeout settles the run, not the claim:
        // the process is still writing to the staging directory until it exits.
        release_install(&owned);
    });
    Ok(harness_runs::get(&run_id)
        .map_or_else(|| serde_json::json!({ "id": run_id }), |run| run.detail()))
}

/// Which stream a reader is draining.
#[derive(Debug, Clone, Copy)]
enum Stream {
    /// The install's own output, which is what an operator watches.
    Stdout,
    /// The install's complaints, which are usually the reason it failed.
    Stderr,
}

/// Start `bash -lc <command>` in its own process group.
///
/// A login shell, because an installer is resolved the way the operator's own
/// shell resolves it: `npm` from a version manager is not on a daemon's bare
/// `PATH` on either platform. The command is a fixed string chosen by harness id;
/// no user input is ever interpolated into it.
fn spawn_install(command: &str) -> std::io::Result<std::process::Child> {
    let mut builder = Command::new("bash");
    builder
        .arg("-lc")
        // A piped installer reports the last stage only, so a failed download
        // would read as a finished install.
        .arg(format!("set -o pipefail; {command}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own group, so the watchdog's one signal ends the installer and whatever
    // it started, rather than only the shell in front of it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        builder.process_group(0);
    }
    builder.spawn()
}

/// Drain one pipe into the registry for as long as it is open.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>, run_id: String, stream: Stream) {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => match stream {
                    Stream::Stdout => harness_runs::append_stdout(&run_id, &buffer[..count]),
                    Stream::Stderr => harness_runs::append_stderr(&run_id, &buffer[..count]),
                },
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Finding things and asking them their version
// ---------------------------------------------------------------------------

/// The executable `binary` in the first of `dirs` that holds one.
///
/// A file is only a candidate when it is executable: a directory that happens to
/// contain a document called `claude` is not an installation, and reporting one
/// would send the operator to a `--version` that never answers.
fn find_on_path(dirs: &[PathBuf], binary: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(binary))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a regular file this process may execute.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The first line one binary prints for `--version`.
///
/// `None` covers every way that can fail: no answer inside
/// [`VERSION_TIMEOUT`], a crash, or output that is not text. An unknown version is
/// a fact about the tool, not an error about the host.
fn version_of(binary: &Path) -> Option<String> {
    let text = run_captured(binary, &["--version"], VERSION_TIMEOUT)?;
    first_line(&text)
}

/// The first non-empty line of `text`, trimmed.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Run one program and capture its stdout, giving up at `timeout`.
///
/// The pipe is read on its own thread so a tool that exits while a child of its
/// own still holds the descriptor cannot turn a version check into a wait; the
/// grace period after the exit collects what the kernel already buffered.
fn run_captured(program: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pipe = child.stdout.take();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(POLL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let bytes = receiver.recv_timeout(PIPE_GRACE).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------
// The Claude tool's sign-in
// ---------------------------------------------------------------------------

/// What one source said about the sign-in.
#[derive(Debug, Clone)]
struct Look {
    /// Where the credential was found.
    source: &'static str,
    /// Whether that source held a token at all.
    found: bool,
    /// Whether the token it held has not expired.
    verified: bool,
    /// The account email it carried, when it carried one.
    account: Option<String>,
}

/// What the Claude tool's sign-in looks like from here.
///
/// The order is the tool's own: the environment variable, then the credentials
/// file, then the macOS keychain. The first *valid* credential wins. When none is
/// valid the answer still names the first source that held one, so an expired
/// sign-in reads as "expired" rather than as "nothing configured".
fn claude_auth(env: &Env) -> Auth {
    let now = now_ms();
    let mut looks: Vec<Look> = Vec::new();
    let first = env
        .stored_token
        .as_deref()
        .map(|token| (SOURCE_DAEMON, token))
        .or(env
            .oauth_token
            .as_deref()
            .map(|token| (SOURCE_ENVIRONMENT, token)));
    if let Some((source, token)) = first {
        let token = token.trim();
        if !token.is_empty() {
            looks.push(Look {
                source,
                found: true,
                // A value that is not a JWT, or is a JWT with no `exp`, cannot be
                // judged here and is taken as still valid: refusing it would make
                // every opaque token look expired.
                verified: !jwt_expired(token).unwrap_or(false),
                account: None,
            });
        }
    }
    if let Some(text) = credentials_file(env) {
        looks.push(look_from_json(SOURCE_CREDENTIALS_FILE, &text, now));
    }
    // The keychain is only asked when the cheap sources did not settle it: on a
    // host where the file answers, a `security` call would cost two seconds to
    // learn nothing.
    if !looks.iter().any(|look| look.verified)
        && env.platform == Platform::MacOs
        && let Some(text) = keychain_credentials(env)
    {
        looks.push(look_from_json(SOURCE_KEYCHAIN, &text, now));
    }
    resolve(looks)
}

/// The one answer a report carries.
fn resolve(looks: Vec<Look>) -> Auth {
    if let Some(hit) = looks.iter().find(|look| look.verified) {
        return Auth {
            kind: OAUTH,
            verified: true,
            account: hit.account.clone(),
            source: hit.source,
        };
    }
    // Not valid anywhere. Naming the source that carried an account is more use
    // than naming the first source that carried a bare token, so an expired
    // credential still tells the operator which account it was.
    let found = looks
        .iter()
        .find(|look| look.found && look.account.is_some())
        .or_else(|| looks.iter().find(|look| look.found));
    match found {
        Some(look) => Auth {
            kind: OAUTH,
            verified: false,
            account: look.account.clone(),
            source: look.source,
        },
        None => Auth {
            kind: OAUTH,
            verified: false,
            account: None,
            source: SOURCE_NONE,
        },
    }
}

/// `~/.claude/.credentials.json`, when it is readable.
fn credentials_file(env: &Env) -> Option<String> {
    let home = env.home.as_ref()?;
    std::fs::read_to_string(home.join(".claude").join(".credentials.json")).ok()
}

/// The Claude sign-in as the macOS keychain holds it, when it holds one.
fn keychain_credentials(env: &Env) -> Option<String> {
    let security = find_on_path(&env.dirs, KEYCHAIN_BINARY)?;
    run_captured(
        &security,
        &["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"],
        KEYCHAIN_TIMEOUT,
    )
}

/// What one credentials blob says, as a [`Look`].
fn look_from_json(source: &'static str, text: &str, now: i64) -> Look {
    match parse_credential(text) {
        Some(credential) => {
            let unexpired = credential
                .expires_at_ms
                .is_none_or(|expires_at| expires_at > now);
            Look {
                source,
                found: credential.token,
                verified: credential.token && unexpired,
                account: credential.account,
            }
        }
        None => Look {
            source,
            found: false,
            verified: false,
            account: None,
        },
    }
}

/// The parts of a credentials blob this module reads.
///
/// The token is a flag here, not a value: it is read to learn *whether* a
/// credential is present and then dropped, because nothing downstream needs it
/// and a struct that held it could leak it into a log line by accident.
#[derive(Debug, Default, PartialEq, Eq)]
struct Credential {
    /// Whether the blob carried a non-empty access token.
    token: bool,
    /// `expiresAt`, in milliseconds since the epoch.
    expires_at_ms: Option<i64>,
    /// The account email, when the blob named one.
    account: Option<String>,
}

/// Read a credentials blob, whether it came from the file or the keychain.
fn parse_credential(text: &str) -> Option<Credential> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    if !root.is_object() {
        return None;
    }
    // The file wraps its fields in `claudeAiOauth`; the keychain item has been
    // seen both ways, so the root stands in for the block when the block is absent.
    let block = root
        .get("claudeAiOauth")
        .filter(|block| block.is_object())
        .unwrap_or(&root);
    let token = block
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|token| !token.trim().is_empty());
    Some(Credential {
        token,
        expires_at_ms: block.get("expiresAt").and_then(epoch_ms),
        account: account_email(block).or_else(|| account_email(&root)),
    })
}

/// A number of milliseconds, from a JSON number or a numeric string.
fn epoch_ms(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|ms| i64::try_from(ms).ok()))
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// The account email under any of the spellings a credentials blob uses.
fn account_email(value: &serde_json::Value) -> Option<String> {
    const PATHS: [&[&str]; 8] = [
        &["account", "emailAddress"],
        &["account", "email"],
        &["account", "email_address"],
        &["user", "emailAddress"],
        &["user", "email"],
        &["emailAddress"],
        &["email"],
        &["email_address"],
    ];
    PATHS.iter().find_map(|path| {
        let mut node = value;
        for key in *path {
            node = node.get(*key)?;
        }
        node.as_str()
            .map(str::trim)
            .filter(|email| !email.is_empty())
            .map(str::to_owned)
    })
}

/// Whether a token's own `exp` has passed, when it carries one.
///
/// `None` means "not a JWT this module can read, or no `exp`": the caller decides
/// what to do with a credential it cannot date, and this function does not pretend
/// it is expired.
fn jwt_expired(token: &str) -> Option<bool> {
    use base64::Engine as _;

    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let expires = claims.get("exp").and_then(serde_json::Value::as_i64)?;
    Some(expires.saturating_mul(1_000) <= now_ms())
}

/// Milliseconds since the Unix epoch, or zero for a clock before it.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// A fixture environment, for tests only
// ---------------------------------------------------------------------------

/// The test seam for [`Env`].
///
/// The substitute is process-wide because [`detect`] is, so the tests that use it
/// also take [`GATE`]: one fixture at a time, and never two tests reading each
/// other's path. Production never touches any of this: it is compiled only for
/// tests and reads only what [`Env::process`] found.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::{Mutex, PoisonError};

    use super::Env;

    /// The fixture in force, if any test put one there.
    static FIXTURE: Mutex<Option<Env>> = Mutex::new(None);

    /// Serialises the tests that need a fixture, because the fixture is
    /// process-wide.
    pub(crate) static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Put `env` in force until the returned guard is dropped.
    pub(crate) fn install(env: Env) -> Fixture {
        *FIXTURE.lock().unwrap_or_else(PoisonError::into_inner) = Some(env);
        Fixture
    }

    /// The fixture in force, cloned out from under the lock.
    pub(super) fn current() -> Option<Env> {
        FIXTURE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Clears the fixture when it goes out of scope, even if the test panicked.
    pub(crate) struct Fixture;

    impl Drop for Fixture {
        fn drop(&mut self) {
            *FIXTURE.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    /// A stub binary in `dir`, with `body` as its whole script.
    fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("stub");
        file.write_all(body.as_bytes()).expect("write stub");
        drop(file);
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
        path
    }

    /// The test environment: these directories, no home, no token, Linux.
    fn env_in(dirs: &[PathBuf]) -> Env {
        Env {
            dirs: dirs.to_vec(),
            home: None,
            oauth_token: None,
            stored_token: None,
            platform: Platform::Linux,
        }
    }

    /// One reported tool, by id.
    fn tool<'a>(report: &'a ToolsReport, id: &str) -> &'a Tool {
        report
            .tools
            .iter()
            .find(|tool| tool.id == id)
            .unwrap_or_else(|| panic!("the report has {id}"))
    }

    /// A base64url JWT whose payload is `{"exp": exp}`, with a header and a
    /// signature that are never read.
    fn jwt(exp: i64) -> String {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{exp}}}"#).as_bytes());
        format!("header.{payload}.signature")
    }

    /// Every harness found on the fixture path reports where it is and the first
    /// line of its own version.
    #[test]
    fn a_hyperlink_wrapped_value_is_lifted_once() {
        // The CLI prints an OSC-8 wrapper: the value appears in the escape
        // payload, then again as the visible text with a BOM between. Returning
        // both run together produced a URL nothing could open.
        let line = "\u{1b}]8;;https://claude.com/cai/oauth?code=true\u{7}\u{feff}https://claude.com/cai/oauth?code=true\u{1b}]8;;\u{7}";
        assert_eq!(
            first_field_with(line, "https://").as_deref(),
            Some("https://claude.com/cai/oauth?code=true")
        );
        let token = "Your token: \u{1b}]8;;\u{7}sk-ant-oat01-abc_DEF\u{7}";
        assert_eq!(
            first_field_with(token, "sk-ant-").as_deref(),
            Some("sk-ant-oat01-abc_DEF")
        );
    }

    /// The rejection the CLI really draws: cursor moves and carriage returns,
    /// and no line break anywhere.
    #[test]
    fn a_redrawn_rejection_with_no_newline_is_read() {
        let drawn = "\u{1b}(B\u{f}\u{1b}[?2004h\r\u{1b}[31C\u{1b}[1A***********klMNOP\r\r\n\u{1b}[2K\u{1b}[1A\u{1b}[2K\u{1b}[G\u{1b}[1A\r\u{1b}[1C\u{1b}[4AOAuth error: Requ\u{1b}[20Gst\u{1b}[23Gfailed with\u{1b}[35Gstatus code 400\u{1b}[K\r\u{1b}[2B\u{1b}[K\r\u{1b}[1C\u{1b}[1BPress\u{1b}[8GEnter\u{1b}[14Gto\u{1b}[17Gretry.\r\u{1b}[1B\u{1b}[K";
        let said = login_complaint(drawn).expect("a rejection");
        assert!(said.contains("refused that code"), "{said}");
        assert!(said.contains("Sign in with Claude again"), "{said}");
    }

    /// The prompt text before a code is entered is not a rejection.
    #[test]
    fn the_prompt_before_a_code_is_not_a_rejection() {
        let prompt = "\u{1b}[2GPaste\u{1b}[8Gcode\u{1b}[13Ghere\u{1b}[18Gif\u{1b}[21Gprompted\u{1b}[30G>\r\r\n";
        assert!(login_complaint(prompt).is_none());
    }

    /// A token cut at a chunk edge is found once the rest arrives, and a link
    /// is not answered until the screen has moved past it.
    #[test]
    fn values_split_across_chunks_are_lifted_whole() {
        let mut seen = String::from("Your token\r\n\u{1b}[32msk-ant-oat01-abc");
        assert_eq!(
            first_field_with(&seen, "sk-ant-").as_deref(),
            Some("sk-ant-oat01-abc")
        );
        seen.push_str("DEF123\u{1b}[0m\r\n");
        assert_eq!(
            first_field_with(&seen, "sk-ant-").as_deref(),
            Some("sk-ant-oat01-abcDEF123")
        );

        let mut link =
            String::from("use the url\r\n\u{1b}]8;;https://claude.com/cai/oauth/authorize?code=tr");
        assert!(
            finished_field(&link, "https://").is_none(),
            "still being drawn"
        );
        link.push_str("ue&state=abc\u{7}https://claude.com/cai/oauth/authorize?code=true&state=abc\u{1b}]8;;\u{7}\r\n");
        assert_eq!(
            finished_field(&link, "https://").as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&state=abc")
        );
    }

    /// A locally-refused code and a server-refused one are different failures
    /// with different fixes, and the phone has to be told which one it hit.
    ///
    /// The real CLI answers "OAuth error: Invalid code. Please make sure the full
    /// code was copied" when the pasted value is not in its `code#state` shape;
    /// that check never leaves the machine. A server rejection says "Request
    /// failed with status code 400". Reading both as "Claude refused that code"
    /// is what sent an operator to fetch a fresh code for a paste they could have
    /// completed.
    /// A rate limit is reported as a rate limit, not as a refused code.
    ///
    /// This is the failure that made the sign-in look broken: the OAuth endpoint
    /// answered 429, the CLI surfaced it as "Request failed with status code 429",
    /// and the old matcher did not recognise that string - so the sign-in ended
    /// either as "Claude refused that code" (sending the operator to fetch a new
    /// code for a server refusing everyone) or, when the CLI printed nothing at
    /// all, as silence.
    #[test]
    fn a_rate_limited_exchange_is_not_a_refused_code() {
        let limited = login_complaint(
            "OAuth error: Requstfailed withstatus code 429 PressEntertoretry.",
        )
        .expect("a complaint");
        assert!(limited.contains("rate-limiting"), "{limited}");
        assert!(limited.contains("Nothing about the code is wrong"), "{limited}");
        assert!(!limited.contains("refused that code"), "{limited}");

        // The structured form the CLI can also print.
        let typed = login_complaint(r#"{"error":{"type":"rate_limit_error"}}"#)
            .expect("a complaint");
        assert!(typed.contains("rate-limiting"), "{typed}");
    }

    #[test]
    fn a_locally_refused_code_is_not_a_server_rejection() {
        let local = login_complaint(
            "OAuth error: Invalidcode. Please makesure the fullcde wascopied PressEntertoretry.",
        )
        .expect("a complaint");
        assert!(local.contains("complete code"), "{local}");
        assert!(local.contains("after the #"), "{local}");
        assert!(!local.contains("refused"), "{local}");

        let server =
            login_complaint("OAuth error: Requstfailed withstatus code 400").expect("a complaint");
        assert!(server.contains("refused that code"), "{server}");
    }

    #[test]
    fn the_cli_own_complaint_is_reported() {
        assert!(login_complaint("OAuth error: Request failed with status code 400").is_some());
        assert!(login_complaint("Invalid code, try again").is_some());
        assert!(login_complaint("Paste code here if prompted >").is_none());
        // The PTY glues words with column moves; the complaint is still read and
        // handed back as one sentence.
        let joined = login_complaint("OAuth\u{1b}[8Gerror: code expired").unwrap_or_default();
        assert!(joined.contains("expired"), "{joined}");
    }

    #[test]
    fn a_fixture_path_reports_each_tool_with_its_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        stub(
            dir.path(),
            "dsh",
            "#!/bin/sh\nprintf 'dsh 2.4.0\\nsecond line\\n'\n",
        );
        stub(
            dir.path(),
            "claude",
            "#!/bin/sh\nprintf '1.9.1 (Claude Code)\\n'\n",
        );
        stub(
            dir.path(),
            "agy",
            "#!/bin/sh\nprintf 'antigravity 0.3\\n'\n",
        );

        let report = detect_in(&env_in(&[dir.path().to_path_buf()]));
        assert_eq!(report.platform.as_str(), "linux");
        assert_eq!(
            report.tools.iter().map(|tool| tool.id).collect::<Vec<_>>(),
            IDS,
            "one row per harness, in picker order"
        );
        for (id, version, hint) in [
            (TOOL_DSH, "dsh 2.4.0", DSH_INSTALL),
            (TOOL_CLAUDE_CODE, "1.9.1 (Claude Code)", CLAUDE_CODE_INSTALL),
            (TOOL_ANTIGRAVITY, "antigravity 0.3", ANTIGRAVITY_INSTALL),
        ] {
            let row = tool(&report, id);
            assert!(row.installed, "{id} is on the fixture path");
            assert_eq!(
                row.version.as_deref(),
                Some(version),
                "{id} reports the first line of --version"
            );
            assert_eq!(
                row.path,
                Some(dir.path().join(row.binary).display().to_string()),
                "{id} reports the absolute path it was found at"
            );
            assert_eq!(row.install_hint, hint);
            assert!(
                !row.installable,
                "{id} has no package manager on this fixture path"
            );
        }
    }

    /// A path without the binaries reports nothing, and still states the hint.
    #[test]
    fn a_path_without_the_binaries_reports_nothing_installed() {
        let dir = tempfile::tempdir().expect("temp dir");
        for env in [env_in(&[dir.path().to_path_buf()]), env_in(&[])] {
            let report = detect_in(&env);
            for row in &report.tools {
                assert!(!row.installed, "{} is not here", row.id);
                assert!(row.version.is_none(), "{} never ran", row.id);
                assert!(row.path.is_none(), "{} has no path", row.id);
                assert!(!row.installable, "{} cannot be installed here", row.id);
                assert!(
                    !row.install_hint.is_empty(),
                    "{} still states the command that would install it",
                    row.id
                );
            }
        }
    }

    /// The version check gives up on a binary that never answers, rather than
    /// leaving a request hanging on it.
    #[test]
    fn a_binary_that_never_answers_has_no_version() {
        let dir = tempfile::tempdir().expect("temp dir");
        stub(dir.path(), "claude", "#!/bin/sh\nexec sleep 30\n");
        let started = Instant::now();
        let report = detect_in(&env_in(&[dir.path().to_path_buf()]));
        let row = tool(&report, TOOL_CLAUDE_CODE);
        assert!(row.installed, "the file is there and executable");
        assert!(row.version.is_none(), "no answer, no version");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the one second deadline ended the check: {:?}",
            started.elapsed()
        );
    }

    /// Installability is the platform plus the package manager that would run the
    /// documented command.
    #[test]
    fn installability_needs_the_documented_package_manager() {
        let dir = tempfile::tempdir().expect("temp dir");
        stub(dir.path(), "npm", "#!/bin/sh\nexit 0\n");
        let linux = detect_in(&env_in(&[dir.path().to_path_buf()]));
        assert!(tool(&linux, TOOL_CLAUDE_CODE).installable, "npm is present");
        assert!(tool(&linux, TOOL_DSH).installable, "npm is present");
        assert!(
            !tool(&linux, TOOL_ANTIGRAVITY).installable,
            "npm does not install it; its installer script needs curl"
        );
        assert_eq!(
            install_plan(&linux, TOOL_CLAUDE_CODE),
            InstallPlan::Start(CLAUDE_CODE_INSTALL.to_owned())
        );

        // macOS runs the same script, so curl is what has to be there.
        let macos = Env {
            platform: Platform::MacOs,
            ..env_in(&[dir.path().to_path_buf()])
        };
        let report = detect_in(&macos);
        assert!(
            !tool(&report, TOOL_ANTIGRAVITY).installable,
            "no curl on the fixture path"
        );
        assert_eq!(
            install_plan(&report, TOOL_ANTIGRAVITY),
            InstallPlan::Unsupported(ANTIGRAVITY_INSTALL_MACOS.to_owned())
        );

        stub(dir.path(), "curl", "#!/bin/sh\nexit 0\n");
        let report = detect_in(&macos);
        assert!(tool(&report, TOOL_ANTIGRAVITY).installable);
        assert_eq!(
            install_plan(&report, TOOL_ANTIGRAVITY),
            InstallPlan::Start(ANTIGRAVITY_INSTALL_MACOS.to_owned())
        );
        // The hint a mac reads is the command a mac would run.
        assert_eq!(
            tool(&report, TOOL_ANTIGRAVITY).install_hint,
            ANTIGRAVITY_INSTALL_MACOS
        );
    }

    /// An install decision is one of four answers, and each names its reason.
    #[test]
    fn an_install_plan_distinguishes_unknown_installed_and_unsupported() {
        let report = detect_in(&env_in(&[]));
        assert_eq!(
            install_plan(&report, "some-other-tool"),
            InstallPlan::Unknown
        );
        assert_eq!(
            install_plan(&report, TOOL_CLAUDE_CODE),
            InstallPlan::Unsupported(CLAUDE_CODE_INSTALL.to_owned()),
            "no npm on the path, so the hint is what an operator runs by hand"
        );

        let windows = Env {
            platform: Platform::Windows,
            ..env_in(&[])
        };
        assert_eq!(
            install_plan(&detect_in(&windows), TOOL_DSH),
            InstallPlan::Unsupported(DSH_INSTALL.to_owned()),
            "a platform this build will not install on"
        );

        let dir = tempfile::tempdir().expect("temp dir");
        stub(dir.path(), "npm", "#!/bin/sh\nexit 0\n");
        stub(dir.path(), "dsh", "#!/bin/sh\nprintf '2.4.0\\n'\n");
        let present = detect_in(&env_in(&[dir.path().to_path_buf()]));
        assert_eq!(install_plan(&present, TOOL_DSH), InstallPlan::Installed);
        assert_eq!(
            install_plan(&present, TOOL_CLAUDE_CODE),
            InstallPlan::Start(CLAUDE_CODE_INSTALL.to_owned())
        );
    }

    /// Every harness accepts its own list, refuses the daemon's catalogue, and
    /// accepts the empty string as "the tool's own default".
    #[test]
    fn a_harness_accepts_only_its_own_models() {
        for (harness, model, accepted) in [
            (TOOL_CLAUDE_CODE, "sonnet", true),
            (TOOL_CLAUDE_CODE, "opus", true),
            (TOOL_CLAUDE_CODE, "haiku", true),
            (TOOL_CLAUDE_CODE, "default", true),
            (TOOL_CLAUDE_CODE, "deepseek-flash", false),
            (TOOL_CLAUDE_CODE, "sonnet ", true),
            (TOOL_ANTIGRAVITY, "gemini-3-pro", true),
            (TOOL_ANTIGRAVITY, "gemini-3-flash", true),
            (TOOL_ANTIGRAVITY, "default", true),
            (TOOL_ANTIGRAVITY, "sonnet", false),
            (TOOL_DSH, "default", true),
            (TOOL_DSH, "sonnet", false),
            (TOOL_DSH, "deepseek-flash", false),
            (TOOL_CLAUDE_CODE, "", true),
            (TOOL_CLAUDE_CODE, "   ", true),
            ("none", "sonnet", false),
            ("none", "", true),
            ("not-a-harness", "default", false),
            ("not-a-harness", "", true),
        ] {
            assert_eq!(
                supports_model(harness, model),
                accepted,
                "{harness} / {model:?}"
            );
        }
        assert_eq!(allowed_models(TOOL_CLAUDE_CODE).len(), 4);
        assert_eq!(allowed_models(TOOL_ANTIGRAVITY).len(), 3);
        assert_eq!(allowed_models(TOOL_DSH), vec!["default"]);
        assert!(allowed_models("none").is_empty());
    }

    /// The credentials file names the account, and the token never appears in the
    /// answer.
    #[test]
    fn a_credentials_file_names_the_account_and_not_the_token() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".claude")).expect("claude dir");
        let expires_at = now_ms() + 3_600_000;
        std::fs::write(
            home.path().join(".claude").join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"secret-token-value","expiresAt":{expires_at},"account":{{"emailAddress":"operator@example.test"}}}}}}"#
            ),
        )
        .expect("credentials");

        let env = Env {
            home: Some(home.path().to_path_buf()),
            ..env_in(&[])
        };
        let report = detect_in(&env);
        let auth = tool(&report, TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("the Claude harness carries the sign-in");
        assert_eq!(
            auth,
            Auth {
                kind: OAUTH,
                verified: true,
                account: Some("operator@example.test".to_owned()),
                source: SOURCE_CREDENTIALS_FILE,
            }
        );
        let body = serde_json::to_string(&auth).expect("json");
        assert!(
            !body.contains("secret-token-value"),
            "the token never leaves the module: {body}"
        );
        assert!(
            !format!("{auth:?}").contains("secret-token-value"),
            "nor into a debug line: {auth:?}"
        );

        // The other two harnesses have no sign-in to report, which is what a
        // client reads as null.
        assert!(tool(&report, TOOL_DSH).auth.is_none());
        assert!(tool(&report, TOOL_ANTIGRAVITY).auth.is_none());
    }

    /// An expired credential is still named, and is not verified.
    #[test]
    fn an_expired_credentials_file_is_not_verified() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".claude")).expect("claude dir");
        std::fs::write(
            home.path().join(".claude").join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a-token","expiresAt":{},"account":{{"emailAddress":"operator@example.test"}}}}}}"#,
                now_ms() - 1_000
            ),
        )
        .expect("credentials");
        let env = Env {
            home: Some(home.path().to_path_buf()),
            ..env_in(&[])
        };
        let auth = tool(&detect_in(&env), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert!(!auth.verified);
        assert_eq!(auth.source, SOURCE_CREDENTIALS_FILE);
        assert_eq!(auth.account.as_deref(), Some("operator@example.test"));
    }

    /// A JWT in the environment counts until its own `exp` passes.
    #[test]
    fn an_environment_token_is_verified_until_it_expires() {
        let seconds = now_ms() / 1_000;
        let token = jwt(seconds + 3_600);
        let env = Env {
            oauth_token: Some(token),
            ..env_in(&[])
        };
        let auth = tool(&detect_in(&env), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert!(auth.verified, "an unexpired JWT verifies");
        assert_eq!(auth.source, SOURCE_ENVIRONMENT);
        assert!(auth.account.is_none(), "the environment names no account");

        let expired = Env {
            oauth_token: Some(jwt(seconds - 3_600)),
            ..env_in(&[])
        };
        let auth = tool(&detect_in(&expired), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert!(!auth.verified);
        assert_eq!(auth.source, SOURCE_ENVIRONMENT);
    }

    /// The three sources are read in the tool's own order: a valid environment
    /// token first, then the credentials file, then the keychain. A stale source
    /// falls through to the next one instead of ending the search.
    #[test]
    fn the_sign_in_falls_through_environment_then_file_then_keychain() {
        let dir = tempfile::tempdir().expect("temp dir");
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".claude")).expect("claude dir");
        let blob = format!(
            r#"{{"claudeAiOauth":{{"accessToken":"file-token","expiresAt":{},"account":{{"emailAddress":"file@example.test"}}}}}}"#,
            now_ms() + 3_600_000
        );
        std::fs::write(home.path().join(".claude").join(".credentials.json"), &blob)
            .expect("credentials");
        // A `security` that answers what the keychain would, with the same blob, so
        // the macOS branch is exercised without a macOS host.
        stub(
            dir.path(),
            KEYCHAIN_BINARY,
            &format!("#!/bin/sh\nprintf '%s\\n' '{blob}'\n"),
        );

        let base = Env {
            dirs: vec![dir.path().to_path_buf()],
            home: Some(home.path().to_path_buf()),
            oauth_token: None,
            stored_token: None,
            platform: Platform::MacOs,
        };
        let seconds = now_ms() / 1_000;

        // A valid environment token wins outright.
        let env = Env {
            oauth_token: Some(jwt(seconds + 3_600)),
            ..base.clone()
        };
        let auth = tool(&detect_in(&env), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert!(auth.verified);
        assert_eq!(auth.source, SOURCE_ENVIRONMENT);

        // An expired one falls through to the file, which is still valid.
        let env = Env {
            oauth_token: Some(jwt(seconds - 3_600)),
            ..base.clone()
        };
        let auth = tool(&detect_in(&env), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert!(auth.verified);
        assert_eq!(
            auth.source, SOURCE_CREDENTIALS_FILE,
            "the file answers before the keychain is asked"
        );
        assert_eq!(auth.account.as_deref(), Some("file@example.test"));

        // With no environment token and no credentials file, the keychain answers.
        let env = Env { home: None, ..base };
        let auth = tool(&detect_in(&env), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert!(auth.verified);
        assert_eq!(auth.source, SOURCE_KEYCHAIN);
        assert_eq!(auth.account.as_deref(), Some("file@example.test"));
        let body = serde_json::to_string(&auth).expect("json");
        assert!(
            !body.contains("file-token"),
            "the keychain token never leaves either: {body}"
        );
    }

    /// No credential anywhere answers `source: "none"` rather than inventing one.
    #[test]
    fn no_credential_anywhere_says_so() {
        let report = detect_in(&env_in(&[]));
        assert_eq!(
            tool(&report, TOOL_CLAUDE_CODE).auth.clone(),
            Some(Auth {
                kind: OAUTH,
                verified: false,
                account: None,
                source: SOURCE_NONE,
            })
        );
        // A credentials file that is not JSON is the same as none at all.
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".claude")).expect("claude dir");
        std::fs::write(
            home.path().join(".claude").join(".credentials.json"),
            "not json",
        )
        .expect("credentials");
        let env = Env {
            home: Some(home.path().to_path_buf()),
            ..env_in(&[])
        };
        let auth = tool(&detect_in(&env), TOOL_CLAUDE_CODE)
            .auth
            .clone()
            .expect("auth");
        assert_eq!(auth.source, SOURCE_NONE);
    }

    /// The wire shape: every documented key, the model lists, and `auth` present
    /// only for the harness that has one.
    #[test]
    fn the_report_serialises_with_every_documented_key() {
        let body = detect_in(&env_in(&[])).json();
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(body["platform"], serde_json::json!("linux"));
        let tools = body["tools"].as_array().expect("tools is an array");
        assert_eq!(tools.len(), 3);

        let mut keys: Vec<&str> = tools[0]
            .as_object()
            .expect("a tool object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "auth",
                "binary",
                "id",
                "install_hint",
                "installable",
                "installed",
                "models",
                "name",
                "path",
                "version",
            ],
            "a tool carries exactly the documented fields"
        );
        assert_eq!(
            tools[0]["models"],
            serde_json::json!([{ "id": "default", "label": "its own default" }])
        );
        assert_eq!(tools[0]["auth"], serde_json::Value::Null);
        assert_eq!(tools[1]["id"], serde_json::json!("claude-code"));
        assert_eq!(tools[1]["auth"]["kind"], serde_json::json!("oauth"));
        let mut auth_keys: Vec<&str> = tools[1]["auth"]
            .as_object()
            .expect("an auth object")
            .keys()
            .map(String::as_str)
            .collect();
        auth_keys.sort_unstable();
        assert_eq!(auth_keys, ["account", "kind", "source", "verified"]);
        assert_eq!(
            tools[2]["models"].as_array().map(Vec::len),
            Some(3),
            "Antigravity offers its own three"
        );
    }

    /// An install is a registered run: it appears in the monitor's table, its
    /// output streams into the tail, and its ending is recorded.
    #[test]
    fn an_install_is_a_registered_run_that_streams_into_the_registry() {
        // A stub command, never a real install: the point is the run record and
        // the two streams, not a package manager.
        let run = start_install(TOOL_CLAUDE_CODE, "printf 'installing\\n'").expect("registered");
        let id = run["id"].as_str().expect("the run has an id").to_owned();
        assert_eq!(run["harness"], serde_json::json!(TOOL_CLAUDE_CODE));
        assert_eq!(
            run["timeout_secs"],
            serde_json::json!(INSTALL_TIMEOUT_SECS),
            "the watchdog bounds an install exactly as it bounds a delegated task"
        );
        assert!(
            run["task"]
                .as_str()
                .is_some_and(|task| task.contains("printf")),
            "the command is the task the monitor shows: {run}"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        let ended = loop {
            let current = harness_runs::get(&id).expect("the run is in the registry");
            if current.finished_at.is_some() {
                break current;
            }
            assert!(
                Instant::now() < deadline,
                "the install run never ended: {current:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(ended.status, RunStatus::Succeeded);
        assert_eq!(ended.exit_code, Some(0));
        assert!(
            ended.stdout_tail.contains("installing"),
            "stdout reached the registry: {:?}",
            ended.stdout_tail
        );
    }
    /// The pty stream is full of escapes; what matters has to survive them.
    #[test]
    /// The code is a pair, not a token. The CLI splits the pasted text on `#`
    /// and exchanges the two halves; cutting the state off here is what made
    /// every sign-in from the phone answer a 400 and read as a refused code.
    #[test]
    fn a_pasted_code_keeps_the_state_half() {
        assert_eq!(code_from_pasted("abc123#stateXYZ"), "abc123#stateXYZ");
        assert_eq!(code_from_pasted("  abc123#stateXYZ  "), "abc123#stateXYZ");
        // A trailing separator with nothing after it is a copy artefact.
        assert_eq!(code_from_pasted("abc123#"), "abc123");
        // A bare code still passes through.
        assert_eq!(code_from_pasted("abc123"), "abc123");
        assert_eq!(code_from_pasted("abc123\n"), "abc123");
    }

    /// A callback URL is unwrapped into the pair, in both directions, so a phone
    /// that pastes the URL and a person who copies the shown code agree.
    #[test]
    fn a_callback_url_becomes_the_pair_the_cli_expects() {
        assert_eq!(
            code_from_pasted(
                "https://platform.claude.com/oauth/code/callback?code=abc123&state=stateXYZ"
            ),
            "abc123#stateXYZ"
        );
        // The order the parameters arrive in does not matter.
        assert_eq!(
            code_from_pasted("http://localhost:54545/callback?state=stateXYZ&code=abc123"),
            "abc123#stateXYZ"
        );
        // A URL with a code and no state is still a code.
        assert_eq!(
            code_from_pasted("https://example.test/cb?code=abc123"),
            "abc123"
        );
        // A percent-encoded state survives decoding.
        assert_eq!(
            code_from_pasted("https://example.test/cb?code=abc123&state=a%2Fb"),
            "abc123#a/b"
        );
    }

    /// `state` is not matched inside a longer key, which a naive substring search
    /// would do on `code_challenge_method` in the same URL.
    #[test]
    fn a_parameter_name_is_matched_at_a_field_boundary() {
        assert_eq!(
            code_from_pasted(
                "https://claude.com/cai/oauth/authorize?code_challenge_method=S256&code=real&state=s1"
            ),
            "real#s1"
        );
        assert_eq!(query_value("a=1&state=s2", "state"), Some("s2".to_owned()));
        assert_eq!(query_value("state=s3&b=2", "state"), Some("s3".to_owned()));
        assert_eq!(query_value("xstate=no", "state"), None);
        assert_eq!(query_value("no-params", "state"), None);
    }

    fn a_sign_in_url_is_lifted_out_of_a_terminal_stream() {
        let line = "\u{1b}]8;id=7e4i2;https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a&state=abc\u{7}https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a&state=abc\u{1b}]8;;\u{7}";
        let found = first_field_with(line, "https://").expect("a url");
        assert_eq!(
            found,
            "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a&state=abc"
        );
    }

    /// A plain pipe has no escapes and still parses.
    #[test]
    fn a_url_without_escapes_still_parses() {
        let found = first_field_with(
            "Use the url below https://claude.com/x, then wait",
            "https://",
        )
        .expect("a url");
        assert_eq!(found, "https://claude.com/x");
    }

    /// The token arrives glued to a prompt and wrapped in escapes.
    #[test]
    fn a_token_is_lifted_out_of_a_terminal_stream() {
        let line = "\u{1b}[32msk-ant-oat01-abcDEF123\u{1b}[0m\r";
        let found = first_field_with(line, "sk-ant-").expect("a token");
        assert_eq!(found, "sk-ant-oat01-abcDEF123");
    }

    /// A line with neither reads as neither, rather than as an empty string.
    #[test]
    fn a_line_without_a_url_reports_nothing() {
        assert!(first_field_with("Welcome to Claude Code", "https://").is_none());
        assert!(first_field_with("", "sk-ant-").is_none());
    }

    /// A truncated escape at the end of a chunk does not eat the text before it.
    #[test]
    fn a_trailing_partial_escape_leaves_the_text_alone() {
        assert_eq!(strip_terminal_escapes("keep\u{1b}["), "keep");
        assert_eq!(strip_terminal_escapes("a\u{1b}]0;title\u{7}b"), "ab");
    }

    /// One install of a harness at a time. The Antigravity installer stages its
    /// download in one fixed path, so a second run overwrites the file the first is
    /// still hashing - which is exactly the "checksum does not match" failure an
    /// operator sees after pressing Install twice.
    #[test]
    fn a_second_install_of_one_harness_is_refused_while_the_first_runs() {
        // Two long sleeps, so the first is certainly still running when the second
        // is asked for. `sleep` writes nothing, so neither run fills a pipe.
        // Names no other test uses: the claim is keyed by the harness id, and a
        // test that borrowed a real one would race the suites beside it.
        const HELD: &str = "testheld-harness";
        const OTHER: &str = "testheld-other";
        let first = start_install(HELD, "sleep 3").expect("the first install starts");
        assert!(first["id"].as_str().is_some_and(|id| !id.is_empty()));

        let second = start_install(HELD, "sleep 3");
        let refused = second.expect_err("a second install is refused");
        assert!(
            refused.contains("already running"),
            "the refusal says what is happening: {refused}"
        );
        assert!(
            refused.contains("stages into one fixed path"),
            "and why: {refused}"
        );

        // A different harness is not blocked by it: the claim is per harness.
        let other = start_install(OTHER, "sleep 1").expect("another harness installs");
        assert!(other["id"].as_str().is_some_and(|id| !id.is_empty()));
    }
}
