//! What a fresh login does not tell us: the account label, and (for Antigravity)
//! the cloud project a turn must be billed to.
//!
//! The token endpoint returns a grant and nothing else. Antigravity's request
//! wrapper needs a `project`, so a login that seals a grant with no project has a
//! token this daemon cannot spend. This module is the one place that turns an
//! access token into the two values [`core_domain::TokenRecord`] stores beside it.
//!
//! Two calls, both copied from the client 9router ships rather than guessed at:
//!
//! | what | where | why |
//! |---|---|---|
//! | `loadCodeAssist` | `cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` | answers `cloudaicompanionProject`, which is the project id |
//! | userinfo | `www.googleapis.com/oauth2/v1/userinfo` | answers `email`, which is the label on the status row |
//!
//! Both authenticate with the access token in an `Authorization: Bearer` header,
//! which is why the transport grew a bearer-carrying form of each verb. Nothing
//! here puts a token in a body.
//!
//! Three rules:
//!
//! - **discovery never fails a login.** A grant that is valid is sealed even when
//!   both calls refuse, because the grant is the thing that took a browser and a
//!   password. The row states what is missing, and `[google] project` in
//!   `oauth-clients.toml` is the manual answer.
//! - **nothing is invented.** `onboardUser` is the call that creates a project for
//!   an account that has never had one. This build does not make it: what it
//!   provisions on the operator's behalf is not something to infer from a minified
//!   bundle. An account that needs it is told so by name.
//! - **the token is an argument, not a field.** Nothing here stores one, and the
//!   note a caller shows carries a status and a sentence, never a body and never a
//!   token.

use super::flow::Provider;
use super::refresh::{HttpRequest, HttpTransport};

/// Where `loadCodeAssist` lives. The `daily-` host serves model traffic; this one
/// serves the project lookup, and the two are not interchangeable.
pub const LOAD_CODE_ASSIST_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";

/// Where a bearer token becomes an email address.
pub const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";

/// The body `loadCodeAssist` is asked to answer for.
///
/// These three strings and `mode: 1` are what the Antigravity surface sends, read
/// out of the client bundle 9router ships. They describe the *kind* of client
/// asking, not a person, and a wrong value is answered with a refusal rather than
/// a grant for a different account.
pub const LOAD_CODE_ASSIST_BODY: &str = r#"{"metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"},"mode":1}"#;

/// What discovery found, and what it could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovered {
    /// The account label the provider reported, when it reported one.
    pub account: Option<String>,
    /// The cloud project, when the provider reported one.
    pub project: Option<String>,
    /// One sentence about what is missing, for the row. Never a token, never a
    /// provider body.
    pub note: Option<String>,
}

impl Discovered {
    /// Whether anything at all was learned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.account.is_none() && self.project.is_none() && self.note.is_none()
    }
}

/// Read `cloudaicompanionProject` out of a `loadCodeAssist` answer.
///
/// The field is a bare string on some answers and an object with an `id` on
/// others, so both are accepted. Anything else is "the provider did not say".
#[must_use]
pub fn project_from_load_code_assist(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let field = parsed.get("cloudaicompanionProject")?;
    if let Some(text) = field.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    let id = field.get("id")?.as_str()?.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// Read `email` out of a userinfo answer.
#[must_use]
pub fn account_from_userinfo(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let email = parsed.get("email")?.as_str()?.trim();
    (!email.is_empty()).then(|| email.to_owned())
}

/// The `loadCodeAssist` request for one access token.
#[must_use]
pub fn load_code_assist_request() -> HttpRequest {
    HttpRequest {
        url: LOAD_CODE_ASSIST_URL.to_owned(),
        content_type: "application/json",
        body: LOAD_CODE_ASSIST_BODY.to_owned(),
    }
}

/// Ask the provider who the account is and which project it is billed to.
///
/// Both calls are attempted, and either may refuse independently: an account with
/// an email but no project is exactly the state a fresh Google login can be in,
/// and it has to be distinguishable from an account with a project but no label.
#[must_use]
pub fn discover(
    transport: &dyn HttpTransport,
    provider: Provider,
    access_token: &str,
) -> Discovered {
    match provider {
        // Anthropic answers a profile endpoint this build has not proven the
        // shape of. A label is cosmetic and a guessed shape puts a wrong account
        // name on a settings row, so Claude logs in with no label rather than a
        // guess. The row says which grant it is by provider instead.
        Provider::Claude => Discovered::default(),
        Provider::Antigravity => {
            let mut found = Discovered::default();
            // The label first: it is the cheap call, and a failure there says
            // nothing about the project.
            if let Ok((status, body)) = transport.get(USERINFO_URL, access_token)
                && (200..300).contains(&status)
            {
                found.account = account_from_userinfo(&body);
            }
            let request = load_code_assist_request();
            match transport.post_bearer(&request, access_token) {
                Ok((status, body)) if (200..300).contains(&status) => {
                    found.project = project_from_load_code_assist(&body);
                    if found.project.is_none() {
                        found.note = Some(
                            "the account answered but named no cloud project, and this build does \
                             not create one. Set `[google] project` in oauth-clients.toml, or sign \
                             in once with the Antigravity app so a project exists."
                                .to_owned(),
                        );
                    }
                }
                Ok((status, _)) => {
                    found.note = Some(format!(
                        "the project lookup was refused with HTTP {status}, so no cloud project \
                         was recorded. Set `[google] project` in oauth-clients.toml."
                    ));
                }
                Err(_) => {
                    found.note = Some(
                        "the project lookup could not be reached, so no cloud project was \
                         recorded. Set `[google] project` in oauth-clients.toml."
                            .to_owned(),
                    );
                }
            }
            found
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::oauth::refresh::TransportError;

    /// A transport that answers by verb and records what it was asked.
    struct Answers {
        userinfo: Option<(u16, String)>,
        load: Option<(u16, String)>,
        get_urls: Mutex<Vec<String>>,
        post_bodies: Mutex<Vec<(String, String)>>,
    }

    impl Answers {
        fn new(userinfo: Option<(u16, &str)>, load: Option<(u16, &str)>) -> Self {
            Self {
                userinfo: userinfo.map(|(status, body)| (status, body.to_owned())),
                load: load.map(|(status, body)| (status, body.to_owned())),
                get_urls: Mutex::new(Vec::new()),
                post_bodies: Mutex::new(Vec::new()),
            }
        }
    }

    impl HttpTransport for Answers {
        fn post(&self, _request: &HttpRequest) -> Result<(u16, String), TransportError> {
            Err(TransportError::Unreachable(
                "discovery posts with a bearer, never plain".to_owned(),
            ))
        }

        fn post_bearer(
            &self,
            request: &HttpRequest,
            bearer: &str,
        ) -> Result<(u16, String), TransportError> {
            self.post_bodies
                .lock()
                .expect("lock")
                .push((request.url.clone(), bearer.to_owned()));
            self.load
                .clone()
                .ok_or_else(|| TransportError::Unreachable("no answer".to_owned()))
        }

        fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), TransportError> {
            self.get_urls
                .lock()
                .expect("lock")
                .push(format!("{url} {bearer}"));
            self.userinfo
                .clone()
                .ok_or_else(|| TransportError::Unreachable("no answer".to_owned()))
        }
    }

    #[test]
    fn a_string_project_and_an_object_project_are_both_read() {
        assert_eq!(
            project_from_load_code_assist(r#"{"cloudaicompanionProject":"aicode-123"}"#).as_deref(),
            Some("aicode-123")
        );
        assert_eq!(
            project_from_load_code_assist(r#"{"cloudaicompanionProject":{"id":"aicode-456"}}"#)
                .as_deref(),
            Some("aicode-456")
        );
        assert_eq!(
            project_from_load_code_assist(r#"{"cloudaicompanionProject":"   "}"#),
            None,
            "blank is not a project"
        );
        assert_eq!(project_from_load_code_assist("{}"), None);
        assert_eq!(project_from_load_code_assist("not json"), None);
    }

    #[test]
    fn the_label_comes_from_the_email_field_only() {
        assert_eq!(
            account_from_userinfo(r#"{"email":"curtis@example.test","name":"X"}"#).as_deref(),
            Some("curtis@example.test")
        );
        assert_eq!(account_from_userinfo(r#"{"name":"X"}"#), None);
        assert_eq!(account_from_userinfo("[]"), None);
    }

    #[test]
    fn the_load_request_names_the_endpoint_and_carries_no_token() {
        let request = load_code_assist_request();
        assert_eq!(request.url, LOAD_CODE_ASSIST_URL);
        assert_eq!(request.content_type, "application/json");
        assert_eq!(request.body, LOAD_CODE_ASSIST_BODY);
        assert!(request.body.contains("IDE_UNSPECIFIED"));
        // The body is valid JSON, which is the only reason to hand-write it.
        let parsed: serde_json::Value =
            serde_json::from_str(&request.body).expect("the body parses");
        assert_eq!(parsed["mode"], 1);
    }

    #[test]
    fn a_full_answer_records_the_account_and_the_project() {
        let transport = Answers::new(
            Some((200, r#"{"email":"curtis@example.test"}"#)),
            Some((200, r#"{"cloudaicompanionProject":"aicode-123"}"#)),
        );
        let found = discover(&transport, Provider::Antigravity, "AT");
        assert_eq!(found.account.as_deref(), Some("curtis@example.test"));
        assert_eq!(found.project.as_deref(), Some("aicode-123"));
        assert!(found.note.is_none());
        assert_eq!(
            transport.get_urls.lock().expect("lock").as_slice(),
            [format!("{USERINFO_URL} AT")]
        );
        assert_eq!(
            transport.post_bodies.lock().expect("lock").as_slice(),
            [(LOAD_CODE_ASSIST_URL.to_owned(), "AT".to_owned())]
        );
    }

    #[test]
    fn a_login_with_a_project_but_no_label_still_reports_the_project() {
        let transport = Answers::new(
            Some((401, r#"{"error":"invalid_token"}"#)),
            Some((200, r#"{"cloudaicompanionProject":{"id":"aicode-9"}}"#)),
        );
        let found = discover(&transport, Provider::Antigravity, "AT");
        assert_eq!(found.account, None);
        assert_eq!(found.project.as_deref(), Some("aicode-9"));
        assert!(found.note.is_none(), "the project is what a turn needs");
    }

    #[test]
    fn an_answer_with_no_project_says_what_to_do_and_does_not_fail() {
        let transport = Answers::new(
            Some((200, r#"{"email":"curtis@example.test"}"#)),
            Some((200, r#"{"allowedTiers":[]}"#)),
        );
        let found = discover(&transport, Provider::Antigravity, "AT-token-value");
        assert_eq!(found.account.as_deref(), Some("curtis@example.test"));
        assert_eq!(found.project, None);
        let note = found.note.expect("a note");
        assert!(note.contains("oauth-clients.toml"), "{note}");
        assert!(
            !note.contains("AT-token-value"),
            "no token in a note: {note}"
        );
    }

    #[test]
    fn a_refused_or_unreachable_lookup_is_a_note_rather_than_a_failed_login() {
        let refused = Answers::new(None, Some((403, r#"{"error":"forbidden"}"#)));
        let found = discover(&refused, Provider::Antigravity, "AT");
        assert_eq!(found.project, None);
        assert!(found.note.expect("a note").contains("403"));

        let unreachable = Answers::new(None, None);
        let found = discover(&unreachable, Provider::Antigravity, "AT");
        assert!(
            found
                .note
                .as_ref()
                .expect("a note")
                .contains("could not be reached"),
            "an unreachable lookup is a note"
        );
        assert!(!found.is_empty());
    }

    #[test]
    fn claude_makes_no_call_at_all() {
        let transport = Answers::new(
            Some((200, r#"{"email":"someone@example.test"}"#)),
            Some((200, r#"{"cloudaicompanionProject":"nope"}"#)),
        );
        let found = discover(&transport, Provider::Claude, "AT");
        assert_eq!(found, Discovered::default());
        assert!(found.is_empty());
        assert!(
            transport.get_urls.lock().expect("lock").is_empty(),
            "a label is not worth an unproven endpoint"
        );
        assert!(transport.post_bodies.lock().expect("lock").is_empty());
    }
}
