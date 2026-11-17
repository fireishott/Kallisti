//! Which application the daemon logs in as, and where the browser comes back to.
//!
//! Both providers this module speaks to use a *public* installed-app client: the
//! client id travels in a URL a person pastes into a browser, and PKCE is what
//! keeps the grant from being replayable. So neither value is a secret in the
//! sense a key is, and neither is written into a settings row. They live in one
//! file so an operator whose client differs can override them without a rebuild:
//!
//! ```text
//! ~/.config/kallisti/oauth-clients.toml   0600, optional
//!
//! [claude]
//! client_id = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
//!
//! [google]
//! client_id = "....apps.googleusercontent.com"
//! client_secret = "GOCSPX-..."
//! project = "aicode-..."          # the cloud project, when discovery cannot read one
//!
//! [flow]
//! callback_port = 7345
//! ```
//!
//! Every value has a documented default, so a host with no file still works. The
//! Google *client secret* is the one value here with no default: it is not
//! published, this daemon does not ship one, and nothing scrapes it out of another
//! program's install. A host that wants an Antigravity login supplies it in this
//! file, which is why the file is 0600. Without it the exchange is sent as a bare
//! public client and Google decides whether that is enough. A file that cannot be
//! parsed is reported by name rather than silently replaced with the defaults.

use std::path::{Path, PathBuf};

/// Anthropic's public PKCE client id.
///
/// Published in the bundle that serves the same grant; identifying the client,
/// not authenticating it. There is no Anthropic client secret for this flow.
pub const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Google's installed-app client id, as used by the Antigravity surface.
pub const GOOGLE_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

/// The scopes Anthropic's OAuth grants, exactly as its own client asks for them.
pub const CLAUDE_SCOPES: [&str; 3] = ["org:create_api_key", "user:profile", "user:inference"];

/// The scopes the Antigravity surface needs.
pub const GOOGLE_SCOPES: [&str; 5] = [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// The loopback port a browser is sent back to when the file names none.
///
/// Deliberately not 7340 (the admin listener) and not 7341 (the device
/// listener): the callback is an unauthenticated GET from a browser, so it gets
/// its own listener with exactly one route rather than a seat inside a documented
/// interface with a session and a trust boundary.
pub const DEFAULT_CALLBACK_PORT: u16 = 7345;

/// Where the browser is sent back to for one port.
#[must_use]
pub fn redirect_uri(port: u16) -> String {
    format!("http://localhost:{port}/callback")
}

/// The client ids and the callback port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clients {
    /// Anthropic's public client id.
    pub claude_client_id: String,
    /// Google's installed-app client id.
    pub google_client_id: String,
    /// Google's installed-app client secret, when this host has one.
    pub google_client_secret: Option<String>,
    /// The Antigravity cloud project to fall back on when the login's own
    /// discovery cannot read one. A project id, not a secret: it names a billing
    /// target, and an operator pasting one is doing what the provider's own app
    /// would have filled in.
    pub google_project: Option<String>,
    /// The port the callback listener binds, loopback only.
    pub callback_port: u16,
    /// The file this was read from, when there was one.
    pub source: Option<PathBuf>,
}

impl Default for Clients {
    fn default() -> Self {
        Self {
            claude_client_id: CLAUDE_CLIENT_ID.to_owned(),
            google_client_id: GOOGLE_CLIENT_ID.to_owned(),
            // Empty by default: a fresh install has no Google secret until the
            // operator supplies one, and a login that needs it says so by name
            // rather than failing as a generic 400.
            google_client_secret: None,
            google_project: None,
            callback_port: DEFAULT_CALLBACK_PORT,
            source: None,
        }
    }
}

impl Clients {
    /// The redirect URI both flows use.
    #[must_use]
    pub fn redirect_uri(&self) -> String {
        redirect_uri(self.callback_port)
    }

    /// The file an operator's overrides live in, under a config directory.
    #[must_use]
    pub fn file_in(config_dir: &Path) -> PathBuf {
        config_dir.join("oauth-clients.toml")
    }
}

/// What went wrong reading the overrides file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientsError {
    /// The file exists and cannot be read.
    #[error("the OAuth client file {} could not be read: {detail}", path.display())]
    Unreadable {
        /// The path.
        path: PathBuf,
        /// The OS message, which never contains a value.
        detail: String,
    },
    /// The file is not valid TOML, or names a field this build does not know.
    #[error("the OAuth client file {} is not valid: {detail}", path.display())]
    Malformed {
        /// The path.
        path: PathBuf,
        /// The parse message, which names fields and not values.
        detail: String,
    },
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    claude: Option<RawClaude>,
    #[serde(default)]
    google: Option<RawGoogle>,
    #[serde(default)]
    flow: Option<RawFlow>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaude {
    #[serde(default)]
    client_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGoogle {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFlow {
    #[serde(default)]
    callback_port: Option<u16>,
}

/// Read the overrides, falling back to the documented defaults.
///
/// A missing file is not an error: it is the shipped state. A file that exists
/// and does not parse is an error, because an operator who edited it wants to
/// know it was not understood rather than silently getting the defaults.
///
/// # Errors
///
/// [`ClientsError`] naming the path and the reason.
pub fn load(config_dir: &Path) -> Result<Clients, ClientsError> {
    let path = Clients::file_in(config_dir);
    let mut clients = Clients::default();
    let raw = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(clients);
        }
        Err(error) => {
            return Err(ClientsError::Unreadable {
                path,
                detail: error.to_string(),
            });
        }
    };
    let parsed: Raw = toml::from_str(&raw).map_err(|error| ClientsError::Malformed {
        path: path.clone(),
        detail: error.to_string(),
    })?;
    if let Some(claude) = parsed.claude
        && let Some(id) = claude.client_id.map(|id| id.trim().to_owned())
        && !id.is_empty()
    {
        clients.claude_client_id = id;
    }
    if let Some(google) = parsed.google {
        if let Some(id) = google.client_id.map(|id| id.trim().to_owned())
            && !id.is_empty()
        {
            clients.google_client_id = id;
        }
        if let Some(secret) = google.client_secret.map(|secret| secret.trim().to_owned())
            && !secret.is_empty()
        {
            clients.google_client_secret = Some(secret);
        }
        if let Some(project) = google.project.map(|project| project.trim().to_owned())
            && !project.is_empty()
        {
            clients.google_project = Some(project);
        }
    }
    if let Some(port) = parsed.flow.and_then(|flow| flow.callback_port) {
        if port == 0 {
            return Err(ClientsError::Malformed {
                path,
                detail: "flow.callback_port must be a real port".to_owned(),
            });
        }
        clients.callback_port = port;
    }
    clients.source = Some(path);
    Ok(clients)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_with_no_file_gets_the_documented_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let clients = load(dir.path()).expect("a missing file is not an error");
        assert_eq!(clients.claude_client_id, CLAUDE_CLIENT_ID);
        assert_eq!(clients.google_client_id, GOOGLE_CLIENT_ID);
        assert_eq!(clients.google_client_secret, None);
        assert_eq!(clients.google_project, None);
        assert_eq!(clients.callback_port, DEFAULT_CALLBACK_PORT);
        assert!(clients.source.is_none());
        assert_eq!(
            clients.redirect_uri(),
            format!("http://localhost:{DEFAULT_CALLBACK_PORT}/callback")
        );
    }

    #[test]
    fn an_override_file_replaces_only_what_it_names() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            Clients::file_in(dir.path()),
            "[google]\nclient_secret = \"GOCSPX-test\"\n\n[flow]\ncallback_port = 7400\n",
        )
        .expect("write");
        let clients = load(dir.path()).expect("parses");
        assert_eq!(clients.google_client_secret.as_deref(), Some("GOCSPX-test"));
        assert_eq!(clients.google_project, None, "the file did not name one");
        assert_eq!(clients.google_client_id, GOOGLE_CLIENT_ID);
        assert_eq!(clients.claude_client_id, CLAUDE_CLIENT_ID);
        assert_eq!(clients.callback_port, 7400);
        assert_eq!(clients.redirect_uri(), "http://localhost:7400/callback");
        assert!(clients.source.is_some());
    }

    #[test]
    fn a_project_named_in_the_file_is_the_manual_answer_for_discovery() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            Clients::file_in(dir.path()),
            "[google]\nproject = \"  aicode-xyz  \"\n",
        )
        .expect("write");
        let clients = load(dir.path()).expect("parses");
        assert_eq!(
            clients.google_project.as_deref(),
            Some("aicode-xyz"),
            "trimmed, because a pasted id usually has whitespace around it"
        );

        // A blank value is the same as no value: a key with nothing in it must
        // not become a project called "".
        std::fs::write(Clients::file_in(dir.path()), "[google]\nproject = \"\"\n").expect("write");
        assert_eq!(load(dir.path()).expect("parses").google_project, None);
        std::fs::write(
            Clients::file_in(dir.path()),
            "[google]\nproject = \"   \"\n",
        )
        .expect("write");
        assert_eq!(load(dir.path()).expect("parses").google_project, None);
    }

    #[test]
    fn a_file_that_does_not_parse_is_reported_by_path_rather_than_ignored() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            Clients::file_in(dir.path()),
            "[claude]\nclient_id = [1,2]\n",
        )
        .expect("write");
        let error = load(dir.path()).expect_err("must be refused");
        let message = error.to_string();
        assert!(message.contains("oauth-clients.toml"), "{message}");

        std::fs::write(Clients::file_in(dir.path()), "[flow]\ncallback_port = 0\n").expect("write");
        assert!(load(dir.path()).is_err(), "port zero is not a port");
    }

    #[test]
    fn the_callback_never_defaults_to_a_listener_that_already_has_a_job() {
        assert_ne!(DEFAULT_CALLBACK_PORT, 7340, "the admin listener");
        assert_ne!(DEFAULT_CALLBACK_PORT, 7341, "the device listener");
        assert_eq!(redirect_uri(7345), "http://localhost:7345/callback");
    }
}
