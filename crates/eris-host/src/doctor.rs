//! `eris doctor` - report what was actually verified, and nothing more.
//!
//! Every field here is something this process checked a moment ago. Nothing is
//! inferred from presence of a file, and no claim is made about a model call:
//! `provider.model_call` says `not_made` because doctor does not spend a token.
//!
//! The report is one JSON object. Top-level `config`, `db` and `provider` are
//! the three verdicts as short strings, so a shell can check
//! `eris doctor --json | jq -e '.db=="ok"'` without walking the detail.

use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::config::{ConfigError, KallistiConfig, LogLevel, Paths, REQUIRED_MODE, TomlConfig};
use crate::db::{Db, TABLES};

/// How long a provider reachability check may take.
const PROVIDER_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// `"ok"` / `"problem"` / finer-grained verdicts, as narrow strings.
pub const OK: &str = "ok";

/// The whole report.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// `ok` when every verdict is `ok`.
    pub status: &'static str,
    /// The binary's version.
    pub version: &'static str,
    /// Verdict for the config files.
    pub config: &'static str,
    /// Verdict for the database.
    pub db: &'static str,
    /// Verdict for the provider configuration.
    pub provider: &'static str,
    /// Verdict for the plaintext-secret scan.
    pub secrets: &'static str,
    /// The evidence behind each verdict.
    pub detail: DoctorDetail,
}

/// What the plaintext scan observed.
///
/// The scan is deliberately dumb: any run of 40 or more characters from
/// `[A-Za-z0-9_-]` in a config or state file is a candidate secret. It reports
/// the count and where it looked, never the matched value, so the report itself
/// cannot leak one.
#[derive(Debug, Serialize)]
pub struct SecretsDetail {
    /// Files read by the scan.
    pub files_scanned: usize,
    /// How many token-shaped literals were found. Zero is the only good number.
    pub plaintext: usize,
    /// `path:line` for each finding, in path order. Values are never copied.
    pub findings: Vec<String>,
    /// Why the scan could not run, when it could not.
    pub error: Option<String>,
}

/// What each check observed.
#[derive(Debug, Serialize)]
pub struct DoctorDetail {
    /// Config file facts.
    pub config: ConfigDetail,
    /// Database facts.
    pub db: DbDetail,
    /// Provider facts. No key value ever appears here.
    pub provider: ProviderDetail,
    /// Plaintext-secret scan facts.
    pub secrets: SecretsDetail,
}

/// The two config files.
#[derive(Debug, Serialize)]
pub struct ConfigDetail {
    /// `~/.config/kallisti/eris.env`
    pub env_file: String,
    /// Its mode, octal, when it exists.
    pub env_mode: Option<String>,
    /// Whether the mode is exactly `0600`.
    pub env_mode_secure: bool,
    /// `~/.config/kallisti/config.toml`
    pub toml_file: String,
    /// Whether that file exists (it is optional).
    pub toml_present: bool,
    /// The non-secret workspace path it resolves to.
    pub workspace_path: String,
    /// Whether the workspace path came from the file or from the default.
    pub workspace_path_is_default: bool,
    /// The configured log level.
    pub log_level: &'static str,
    /// The error, when the secret half did not load.
    pub error: Option<String>,
}

/// The database.
#[derive(Debug, Serialize)]
pub struct DbDetail {
    /// `~/.local/share/kallisti/eris.db`
    pub path: String,
    /// `wal` when configured correctly.
    pub journal_mode: String,
    /// Whether this connection enforces foreign keys.
    pub foreign_keys: bool,
    /// Configured busy timeout, milliseconds.
    pub busy_timeout_ms: i64,
    /// Highest applied migration.
    pub schema_version: i64,
    /// How many migrations are recorded, to catch a double-apply.
    pub applied_migrations: i64,
    /// `PRAGMA quick_check`.
    pub quick_check: String,
    /// How many of the nine expected tables are present.
    pub tables_present: usize,
    /// How many were expected.
    pub tables_expected: usize,
    /// Tables that are missing, by name.
    pub tables_missing: Vec<String>,
    /// `setup` or `ready`.
    pub setup_state: String,
    /// The error, when the database did not open.
    pub error: Option<String>,
}

/// The provider seam as far as it can be checked without spending a token.
#[derive(Debug, Serialize)]
pub struct ProviderDetail {
    /// The OpenAI-compatible base URL.
    pub base_url: String,
    /// The model id.
    pub model: String,
    /// The key reference, e.g. `env:NINEROUTER_API_KEY`. Not a key.
    pub key_ref: String,
    /// Whether that variable is set in *this* process environment.
    pub key_present: bool,
    /// `host:port` derived from `base_url`, when it parsed.
    pub tcp_target: Option<String>,
    /// Whether a TCP connection was accepted.
    pub tcp_reachable: Option<bool>,
    /// Always `not_made`: doctor never calls the model.
    pub model_call: &'static str,
    /// The error, when the provider half did not load.
    pub error: Option<String>,
}

impl DoctorReport {
    /// Whether every verdict is `ok`.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.status == OK
    }

    /// Render the report as one compact JSON object.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|error| {
            format!("{{\"status\":\"error\",\"message\":\"cannot serialise the report: {error}\"}}")
        })
    }

    /// Render the report for a human.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("eris doctor {}\n", self.version));
        out.push_str(&format!("  config   {}\n", self.config));
        out.push_str(&format!("  db       {}\n", self.db));
        out.push_str(&format!("  provider {}\n", self.provider));
        out.push_str(&format!("  secrets  {}\n", self.secrets));
        out.push('\n');

        let config = &self.detail.config;
        out.push_str(&format!("  env file      {}\n", config.env_file));
        out.push_str(&format!(
            "  env mode      {}\n",
            config.env_mode.as_deref().unwrap_or("missing")
        ));
        out.push_str(&format!("  config.toml   {}\n", config.toml_file));
        out.push_str(&format!(
            "  workspace     {}{}\n",
            config.workspace_path,
            if config.workspace_path_is_default {
                " (default)"
            } else {
                ""
            }
        ));
        out.push_str(&format!("  log level     {}\n", config.log_level));

        let db = &self.detail.db;
        out.push_str(&format!("  database      {}\n", db.path));
        out.push_str(&format!(
            "  journal mode  {}  foreign_keys {}  busy_timeout {}ms\n",
            db.journal_mode, db.foreign_keys, db.busy_timeout_ms
        ));
        out.push_str(&format!(
            "  schema        v{} ({} migrations)  quick_check {}\n",
            db.schema_version, db.applied_migrations, db.quick_check
        ));
        out.push_str(&format!(
            "  tables        {}/{} present  setup_state {}\n",
            db.tables_present, db.tables_expected, db.setup_state
        ));
        if !db.tables_missing.is_empty() {
            out.push_str(&format!(
                "  missing       {}\n",
                db.tables_missing.join(", ")
            ));
        }

        let provider = &self.detail.provider;
        out.push_str(&format!("  base url      {}\n", provider.base_url));
        out.push_str(&format!("  model         {}\n", provider.model));
        out.push_str(&format!(
            "  key ref       {} (present: {})\n",
            provider.key_ref, provider.key_present
        ));
        out.push_str(&format!(
            "  reachable     {}\n",
            match provider.tcp_reachable {
                Some(true) => "yes",
                Some(false) => "no",
                None => "not checked (base_url did not parse)",
            }
        ));
        out.push_str("  model call    not made (doctor spends no token)\n");

        let secrets = &self.detail.secrets;
        out.push_str(&format!(
            "  plaintext     {} token-shaped literal(s) across {} file(s)\n",
            secrets.plaintext, secrets.files_scanned
        ));
        for finding in &secrets.findings {
            out.push_str(&format!("  finding       {finding}\n"));
        }

        if let Some(error) = &self.status_error() {
            out.push_str(&format!("\n  error: {error}\n"));
        }
        out
    }

    fn status_error(&self) -> Option<String> {
        self.detail
            .config
            .error
            .clone()
            .or_else(|| self.detail.db.error.clone())
            .or_else(|| self.detail.provider.error.clone())
            .or_else(|| self.detail.secrets.error.clone())
    }
}

/// Run every check. Never returns `Err`: a broken environment is the thing being
/// reported.
#[must_use]
pub fn run(paths: &Paths) -> DoctorReport {
    let (config_verdict, config_detail, eris) = check_config(paths);
    let (db_verdict, db_detail) = check_db(paths);
    let (provider_verdict, provider_detail) = check_provider(eris.as_ref());
    let (secrets_verdict, secrets_detail) = check_secrets(paths);

    let status = if config_verdict == OK
        && db_verdict == OK
        && provider_verdict == OK
        && secrets_verdict == OK
    {
        OK
    } else {
        "degraded"
    };

    DoctorReport {
        status,
        version: crate::admin::VERSION,
        config: config_verdict,
        db: db_verdict,
        provider: provider_verdict,
        secrets: secrets_verdict,
        detail: DoctorDetail {
            config: config_detail,
            db: db_detail,
            provider: provider_detail,
            secrets: secrets_detail,
        },
    }
}

/// How long the longest clean literal may be before it is reported.
const PLAINTEXT_RUN: usize = 40;

/// The biggest file the scan will read. Anything larger is reported, not read.
const PLAINTEXT_FILE_CEILING: u64 = 256 * 1024;

/// Scan the config and state directories for token-shaped plaintext.
///
/// A candidate is any run of [`PLAINTEXT_RUN`] or more characters from
/// `[A-Za-z0-9_-]`. The database, its sidecars, and any copy of it are skipped:
/// they hold Argon2id verifiers by design, and a verifier is not a usable
/// secret. The scan reports where it looked, never what it found.
fn check_secrets(paths: &Paths) -> (&'static str, SecretsDetail) {
    let mut detail = SecretsDetail {
        files_scanned: 0,
        plaintext: 0,
        findings: Vec::new(),
        error: None,
    };

    let db_name = paths
        .db_file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();

    for dir in [&paths.config_dir, &paths.state_dir] {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                detail.error = Some(format!("cannot list {}: {error}", dir.display()));
                return ("cannot_scan", detail);
            }
        };

        let mut files: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file() && !is_database_file(path, &db_name))
            .collect();
        files.sort();

        for path in files {
            match fs::metadata(&path) {
                Ok(meta) if meta.len() > PLAINTEXT_FILE_CEILING => {
                    detail.findings.push(format!(
                        "{}:0 (larger than {PLAINTEXT_FILE_CEILING} bytes, not read)",
                        path.display()
                    ));
                    detail.plaintext += 1;
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    detail.error = Some(format!("cannot stat {}: {error}", path.display()));
                    return ("cannot_scan", detail);
                }
            }

            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            detail.files_scanned += 1;
            let text = String::from_utf8_lossy(&bytes);
            for (index, line) in text.lines().enumerate() {
                if longest_run(line) >= PLAINTEXT_RUN {
                    detail.plaintext += 1;
                    detail
                        .findings
                        .push(format!("{}:{}", path.display(), index + 1));
                }
            }
        }
    }

    let verdict = if detail.plaintext == 0 {
        OK
    } else {
        "plaintext_secret"
    };
    (verdict, detail)
}

/// Whether a path is the database, a sidecar, or a copy of it.
///
/// Everything whose name starts with the configured database file name is
/// skipped: `eris.db`, `eris.db-wal`, `eris.db-shm`, and any `eris.db.*` backup a
/// human or a test left behind. Those are verifier stores, not secret files, and
/// reading them as text produces base64 noise rather than findings.
fn is_database_file(path: &Path, db_name: &str) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    !db_name.is_empty() && name.starts_with(db_name)
}

/// The longest run of `[A-Za-z0-9_-]` in one line.
fn longest_run(line: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for byte in line.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn check_config(paths: &Paths) -> (&'static str, ConfigDetail, Option<KallistiConfig>) {
    let env_mode = mode_string(&paths.env_file);
    let env_mode_secure = env_mode == Some(format!("{REQUIRED_MODE:04o}"));
    let toml = TomlConfig::load(&paths.toml_file);
    let toml_present = paths.toml_file.is_file();

    let (verdict, error, eris) = match KallistiConfig::load(&paths.env_file) {
        Ok(config) => (OK, None, Some(config)),
        Err(error) => {
            let verdict = match &error {
                ConfigError::InsecurePermissions { .. } => "insecure_permissions",
                _ => "invalid",
            };
            (verdict, Some(error.to_string()), None)
        }
    };

    let (toml_error, workspace_path, workspace_path_is_default, log_level) = match toml {
        Ok(config) => (
            None,
            config.workspace_path().display().to_string(),
            config.workspace_path_is_default(),
            config.log_level(),
        ),
        Err(error) => (
            Some(error.to_string()),
            String::from("<unavailable>"),
            true,
            LogLevel::DEFAULT,
        ),
    };

    let error = error.or(toml_error);
    let verdict = if error.is_some() && verdict == OK {
        "invalid"
    } else {
        verdict
    };

    (
        verdict,
        ConfigDetail {
            env_file: paths.env_file.display().to_string(),
            env_mode,
            env_mode_secure,
            toml_file: paths.toml_file.display().to_string(),
            toml_present,
            workspace_path,
            workspace_path_is_default,
            log_level: log_level.as_str(),
            error,
        },
        eris,
    )
}

fn check_db(paths: &Paths) -> (&'static str, DbDetail) {
    let mut detail = DbDetail {
        path: paths.db_file.display().to_string(),
        journal_mode: String::from("unknown"),
        foreign_keys: false,
        busy_timeout_ms: 0,
        schema_version: 0,
        applied_migrations: 0,
        quick_check: String::from("unknown"),
        tables_present: 0,
        tables_expected: TABLES.len(),
        tables_missing: Vec::new(),
        setup_state: String::from("unknown"),
        error: None,
    };

    let db = match Db::open(&paths.db_file) {
        Ok(db) => db,
        Err(error) => {
            detail.error = Some(error.to_string());
            return ("cannot_open", detail);
        }
    };

    let observed = (|| -> Result<(), crate::db::DbError> {
        detail.journal_mode = db.journal_mode()?;
        detail.foreign_keys = db.foreign_keys_enabled()?;
        detail.busy_timeout_ms = db.busy_timeout_ms()?;
        detail.schema_version = db.schema_version()?;
        detail.applied_migrations = db.applied_migration_count()?;
        detail.quick_check = db.quick_check()?;
        let present = db.table_names()?;
        detail.tables_missing = TABLES
            .iter()
            .filter(|expected| !present.iter().any(|name| name == *expected))
            .map(|name| (*name).to_owned())
            .collect();
        detail.tables_present = TABLES.len() - detail.tables_missing.len();
        detail.setup_state = db.setup_state()?.as_str().to_owned();
        Ok(())
    })();

    if let Err(error) = observed {
        detail.error = Some(error.to_string());
        return ("unusable", detail);
    }

    let healthy = detail.journal_mode == "wal"
        && detail.foreign_keys
        && detail.quick_check == "ok"
        && detail.tables_missing.is_empty();
    (if healthy { OK } else { "unhealthy" }, detail)
}

fn check_provider(eris: Option<&KallistiConfig>) -> (&'static str, ProviderDetail) {
    let Some(config) = eris else {
        return (
            "unconfigured",
            ProviderDetail {
                base_url: String::from("<unavailable>"),
                model: String::from("<unavailable>"),
                key_ref: String::from("<unavailable>"),
                key_present: false,
                tcp_target: None,
                tcp_reachable: None,
                model_call: "not_made",
                error: Some(String::from(
                    "eris.env did not load, so there is no provider to check",
                )),
            },
        );
    };

    // Presence only. The value is read from the process environment and dropped;
    // it is never printed, stored or compared.
    let key_present = std::env::var(config.api_key_ref().env_name())
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let tcp_target = host_port(config.base_url());
    let tcp_reachable = tcp_target.as_deref().map(tcp_reachable);

    let verdict = if !key_present {
        "key_missing"
    } else if tcp_reachable == Some(false) {
        "unreachable"
    } else {
        OK
    };

    (
        verdict,
        ProviderDetail {
            base_url: config.base_url().to_owned(),
            model: config.model().to_owned(),
            key_ref: config.api_key_ref().wire(),
            key_present,
            tcp_target,
            tcp_reachable,
            model_call: "not_made",
            error: None,
        },
    )
}

/// `http://host:port/path` -> `host:port`. Only `http`/`https`, only a literal
/// host, no default ports beyond 80/443.
fn host_port(base_url: &str) -> Option<String> {
    let (scheme, rest) = base_url.split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split('/').next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    if authority.parse::<std::net::SocketAddr>().is_ok() {
        return Some(authority.to_owned());
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
            Some(format!("{host}:{port}"))
        }
        Some(_) => None,
        None if !authority.is_empty() => Some(format!("{authority}:{default_port}")),
        None => None,
    }
}

/// A short TCP connect. Reaching the port is not a model call; it only says the
/// gateway is listening.
fn tcp_reachable(target: &str) -> bool {
    let Ok(addrs) = target.to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, PROVIDER_PROBE_TIMEOUT).is_ok())
}

/// The octal mode of a path, when it exists.
fn mode_string(path: &Path) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(format!("{:04o}", metadata.permissions().mode() & 0o777))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// Convenience for `main`: resolve the paths, then report.
#[must_use]
pub fn run_default() -> DoctorReport {
    match Paths::from_home() {
        Ok(paths) => run(&paths),
        Err(error) => DoctorReport {
            status: "degraded",
            version: crate::admin::VERSION,
            config: "invalid",
            db: "unknown",
            provider: "unconfigured",
            secrets: "unknown",
            detail: DoctorDetail {
                config: ConfigDetail {
                    env_file: String::from("<unknown>"),
                    env_mode: None,
                    env_mode_secure: false,
                    toml_file: String::from("<unknown>"),
                    toml_present: false,
                    workspace_path: String::from("<unknown>"),
                    workspace_path_is_default: true,
                    log_level: LogLevel::DEFAULT.as_str(),
                    error: Some(error.to_string()),
                },
                db: DbDetail {
                    path: String::from("<unknown>"),
                    journal_mode: String::from("unknown"),
                    foreign_keys: false,
                    busy_timeout_ms: 0,
                    schema_version: 0,
                    applied_migrations: 0,
                    quick_check: String::from("unknown"),
                    tables_present: 0,
                    tables_expected: TABLES.len(),
                    tables_missing: Vec::new(),
                    setup_state: String::from("unknown"),
                    error: Some(String::from("paths could not be resolved")),
                },
                provider: ProviderDetail {
                    base_url: String::from("<unknown>"),
                    model: String::from("<unknown>"),
                    key_ref: String::from("<unknown>"),
                    key_present: false,
                    tcp_target: None,
                    tcp_reachable: None,
                    model_call: "not_made",
                    error: Some(String::from("paths could not be resolved")),
                },
                secrets: SecretsDetail {
                    files_scanned: 0,
                    plaintext: 0,
                    findings: Vec::new(),
                    error: Some(String::from("paths could not be resolved")),
                },
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_understands_the_documented_base_url() {
        assert_eq!(
            host_port("http://127.0.0.1:20128/v1").as_deref(),
            Some("127.0.0.1:20128")
        );
        assert_eq!(
            host_port("https://api.example.test/v1").as_deref(),
            Some("api.example.test:443")
        );
        assert_eq!(
            host_port("http://localhost/v1").as_deref(),
            Some("localhost:80")
        );
        assert_eq!(host_port("not a url"), None);
        assert_eq!(host_port("ftp://example.test"), None);
    }

    #[test]
    fn the_report_serialises_with_top_level_verdicts() {
        let report = DoctorReport {
            status: OK,
            version: "1.0.0",
            config: OK,
            db: OK,
            provider: OK,
            secrets: OK,
            detail: DoctorDetail {
                config: ConfigDetail {
                    env_file: String::from("/tmp/eris.env"),
                    env_mode: Some(String::from("0600")),
                    env_mode_secure: true,
                    toml_file: String::from("/tmp/config.toml"),
                    toml_present: false,
                    workspace_path: String::from("/tmp/ws"),
                    workspace_path_is_default: true,
                    log_level: "info",
                    error: None,
                },
                db: DbDetail {
                    path: String::from("/tmp/eris.db"),
                    journal_mode: String::from("wal"),
                    foreign_keys: true,
                    busy_timeout_ms: 5000,
                    schema_version: 1,
                    applied_migrations: 1,
                    quick_check: String::from("ok"),
                    tables_present: 9,
                    tables_expected: 9,
                    tables_missing: Vec::new(),
                    setup_state: String::from("setup"),
                    error: None,
                },
                provider: ProviderDetail {
                    base_url: String::from("http://127.0.0.1:20128/v1"),
                    model: String::from("ds/deepseek-v4-flash"),
                    key_ref: String::from("env:NINEROUTER_API_KEY"),
                    key_present: true,
                    tcp_target: Some(String::from("127.0.0.1:20128")),
                    tcp_reachable: Some(true),
                    model_call: "not_made",
                    error: None,
                },
                secrets: SecretsDetail {
                    files_scanned: 2,
                    plaintext: 0,
                    findings: Vec::new(),
                    error: None,
                },
            },
        };

        let json: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("valid json object");
        assert_eq!(json["db"], "ok", "top-level db verdict, as documented");
        assert_eq!(json["config"], "ok");
        assert_eq!(json["provider"], "ok");
        assert_eq!(json["detail"]["db"]["tables_expected"], 9);
        assert_eq!(json["detail"]["provider"]["model_call"], "not_made");
        assert_eq!(json["secrets"], "ok", "top-level secrets verdict");
        assert_eq!(json["detail"]["secrets"]["plaintext"], 0);
        assert!(report.is_ok());
    }

    #[test]
    fn the_longest_run_counts_only_the_documented_alphabet() {
        assert_eq!(longest_run("short_value"), 11);
        assert_eq!(longest_run("a-b_c"), 5, "hyphen and underscore count");
        assert_eq!(longest_run("dots.and/slashes break it"), 7, "slashes");
        assert_eq!(longest_run(""), 0);
        let forty = "a".repeat(PLAINTEXT_RUN);
        assert_eq!(longest_run(&forty), PLAINTEXT_RUN);
    }

    #[test]
    fn the_scan_finds_a_planted_literal_and_ignores_the_database() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config_dir = dir.path().join("config");
        let state_dir = dir.path().join("state");
        fs::create_dir_all(&config_dir).expect("config dir");
        fs::create_dir_all(&state_dir).expect("state dir");

        let secret = "a".repeat(PLAINTEXT_RUN + 5);
        fs::write(config_dir.join("leaked.env"), format!("TOKEN={secret}\n"))
            .expect("write the planted file");
        // A verifier lives in the database by design, so the scan must skip it.
        fs::write(state_dir.join("eris.db"), format!("{secret}\n")).expect("write the db");
        // So must a stale copy of it, which is base64 noise rather than a secret.
        fs::write(state_dir.join("eris.db.pre-p7"), format!("{secret}\n")).expect("write a copy");
        fs::write(state_dir.join("first-run.txt"), "hush drift creme pogo\n").expect("clean file");

        let paths = Paths {
            config_dir: config_dir.clone(),
            env_file: config_dir.join("eris.env"),
            toml_file: config_dir.join("config.toml"),
            state_dir: state_dir.clone(),
            db_file: state_dir.join("eris.db"),
            token_dir: dir.path().join("config/tokens"),
            token_key_file: dir.path().join("config/keys/token.key"),
        };

        let (verdict, detail) = check_secrets(&paths);
        assert_eq!(verdict, "plaintext_secret");
        assert_eq!(detail.plaintext, 1, "only the leaked config file counts");
        assert_eq!(detail.files_scanned, 2, "leaked.env and first-run.txt");
        assert_eq!(detail.findings.len(), 1);
        assert!(
            detail.findings[0].ends_with("leaked.env:1"),
            "the finding names the file and line, never the value: {:?}",
            detail.findings
        );
        assert!(
            !detail.findings[0].contains(&secret),
            "the report must not copy the secret"
        );
    }

    #[test]
    fn a_clean_config_tree_scans_ok() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config_dir = dir.path().join("config");
        let state_dir = dir.path().join("state");
        fs::create_dir_all(&config_dir).expect("config dir");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::write(
            config_dir.join("eris.env"),
            "KALLISTI_API_KEY_REF=env:NINEROUTER_API_KEY\n",
        )
        .expect("write env");
        fs::write(
            state_dir.join("first-run.txt"),
            "hush drift creme pogo foyer mule\n",
        )
        .expect("write the passphrase file");

        let paths = Paths {
            config_dir: config_dir.clone(),
            env_file: config_dir.join("eris.env"),
            toml_file: config_dir.join("config.toml"),
            state_dir: state_dir.clone(),
            db_file: state_dir.join("eris.db"),
            token_dir: dir.path().join("config/tokens"),
            token_key_file: dir.path().join("config/keys/token.key"),
        };

        let (verdict, detail) = check_secrets(&paths);
        assert_eq!(verdict, OK, "findings: {:?}", detail.findings);
        assert_eq!(detail.plaintext, 0);
        assert_eq!(detail.files_scanned, 2);
    }
}
