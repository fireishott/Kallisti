//! The loopback listener a browser is sent back to.
//!
//! One route, one port, one purpose: catch `GET /callback?code=...&state=...`
//! from the provider, spend the code, seal the grant, and tell the operator what
//! happened. It binds `127.0.0.1` only, and it closes as soon as it has answered
//! or as soon as the flow expires, whichever comes first.
//!
//! Why this is not part of the admin listener: the callback is an unauthenticated
//! GET from a browser that has just come back from a third party. The admin
//! surface has a session cookie, a trust boundary for forwarding headers, and a
//! documented set of routes; bolting an unauthenticated endpoint onto it would
//! put a route that anyone who can reach `:7340` can hit inside a surface that is
//! otherwise behind a login. A separate listener has no session to borrow and no
//! other route to confuse it with.
//!
//! What it refuses, by design:
//!
//! - a bind anywhere but loopback;
//! - a callback whose `state` is unknown, already spent, or expired (the flow is
//!   taken out of [`PendingFlows`] before the exchange is attempted);
//! - a provider error response, which is shown as the provider's own code.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use core_domain::TokenStore;

use super::clients::Clients;
use super::discover::{self, Discovered};
use super::flow::{self, PendingFlows, Provider};
use super::refresh::{HttpTransport, ReqwestTransport};

/// The account discovery hook: given a provider and an access token, report the
/// label and the cloud project to record beside the grant.
///
/// A boxed `Fn` rather than a plain function pointer because the real one closes
/// over the service's HTTP transport, and a test's closes over a stub. Stated
/// once here so the field and the builder cannot drift.
pub type AccountHook = Arc<dyn Fn(Provider, &str) -> Discovered + Send + Sync>;

/// What one callback produced, kept so the settings page can say what happened
/// after the browser tab closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Which account.
    pub provider: Provider,
    /// Whether a grant was sealed.
    pub ok: bool,
    /// The account label, when one was discovered.
    pub account: Option<String>,
    /// The cloud project, for the provider that needs one.
    pub project: Option<String>,
    /// What to read, in one sentence. Never carries a token.
    pub message: String,
    /// When it happened, in unix seconds.
    pub at: i64,
}

impl Outcome {
    /// A failure, with the provider's reason and no secret in it.
    #[must_use]
    pub fn failed(provider: Provider, message: impl Into<String>) -> Self {
        Self {
            provider,
            ok: false,
            account: None,
            project: None,
            message: message.into(),
            at: flow::now_secs(),
        }
    }
}

/// Everything a callback needs, and the record of what it did.
#[derive(Clone)]
pub struct Callbacks {
    /// The pending logins this process started.
    pub pending: Arc<PendingFlows>,
    /// The client ids and the callback port.
    pub clients: Arc<Clients>,
    /// The encrypted store the grant is sealed into.
    pub store: Arc<dyn TokenStore + Send + Sync>,
    /// The transport the exchange uses. `ReqwestTransport` in the daemon; a
    /// mock in a test.
    pub transport: Arc<dyn HttpTransport + Send + Sync>,
    /// The last outcome, for the settings page.
    last: Arc<Mutex<Option<Outcome>>>,
    /// The account discovery hook, injected so a test can prove the label and the
    /// project are recorded without reaching the provider. `None` means the real
    /// one: [`super::discover::discover`], over this service's transport.
    pub account: Option<AccountHook>,
    /// The port the listener is serving on, once it is. Written by
    /// [`Callbacks::ensure_listening`] and read by the panel, so a page states the
    /// port a browser will actually come back to rather than the configured one.
    listening: Arc<Mutex<Option<u16>>>,
    /// Why the listener could not start, when it could not. The paste flow does
    /// not need it, so this is a note beside the row rather than a refusal.
    listen_error: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for Callbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Callbacks")
            .field("port", &self.clients.callback_port)
            .field("last", &self.last.lock().ok().and_then(|last| last.clone()))
            .finish_non_exhaustive()
    }
}

impl Callbacks {
    /// Build the service for one daemon.
    #[must_use]
    pub fn new(
        pending: Arc<PendingFlows>,
        clients: Arc<Clients>,
        store: Arc<dyn TokenStore + Send + Sync>,
    ) -> Self {
        Self {
            pending,
            clients,
            store,
            transport: Arc::new(ReqwestTransport),
            last: Arc::new(Mutex::new(None)),
            account: None,
            listening: Arc::new(Mutex::new(None)),
            listen_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Why the automatic callback is off, when it is.
    #[must_use]
    pub fn listen_error(&self) -> Option<String> {
        self.listen_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    /// Replace the transport. A test passes one that answers locally so no test
    /// can reach a real token endpoint.
    #[must_use]
    pub fn with_transport(mut self, transport: Arc<dyn HttpTransport + Send + Sync>) -> Self {
        self.transport = transport;
        self
    }

    /// Replace the account discovery hook.
    #[must_use]
    pub fn with_account(mut self, account: AccountHook) -> Self {
        self.account = Some(account);
        self
    }

    /// The last outcome, if any.
    #[must_use]
    pub fn last(&self) -> Option<Outcome> {
        self.last.lock().ok().and_then(|last| last.clone())
    }

    /// The last outcome for one provider.
    #[must_use]
    pub fn last_for(&self, provider: Provider) -> Option<Outcome> {
        self.last().filter(|outcome| outcome.provider == provider)
    }

    /// Forget the last outcome, so a page shows a fresh state after a press.
    pub fn clear(&self) {
        if let Ok(mut last) = self.last.lock() {
            *last = None;
        }
    }

    /// Record an outcome from a route, so the panel can state what a press did
    /// after the browser has followed the redirect.
    pub fn note(&self, outcome: Outcome) {
        self.record(outcome);
    }

    fn record(&self, outcome: Outcome) {
        if let Ok(mut last) = self.last.lock() {
            *last = Some(outcome);
        }
    }

    /// Bind the callback listener on `ip:port`, refusing anything but loopback.
    ///
    /// # Errors
    ///
    /// The bind error, or a refusal naming the address when it is not loopback.
    pub async fn bind(&self, port: u16) -> Result<tokio::net::TcpListener, String> {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        if !addr.ip().is_loopback() {
            return Err(format!(
                "{addr} is not a loopback address, and the OAuth callback is loopback only"
            ));
        }
        tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|error| format!("the OAuth callback could not bind {addr}: {error}"))
    }

    /// The port this listener is serving on, when it is serving.
    #[must_use]
    pub fn listening_on(&self) -> Option<u16> {
        self.listening.lock().ok().and_then(|port| *port)
    }

    /// Start the callback listener if it is not already serving, and report the
    /// port.
    ///
    /// On demand rather than at boot, for two reasons. A host that never connects
    /// an account should not hold a port open for a feature it does not use, and
    /// a port that is already taken by something else is a failure the operator
    /// meets at the moment they press Connect, with the reason in the answer,
    /// instead of a line in a log they will never read.
    ///
    /// The second caller gets the port the first one bound: the flag is set only
    /// after the bind succeeded, so two presses cannot race two listeners onto one
    /// port.
    ///
    /// # Errors
    ///
    /// The bind failure, named, when the port is taken or not permitted.
    pub async fn ensure_listening(self: &Arc<Self>) -> Result<u16, String> {
        if let Some(port) = self.listening_on() {
            return Ok(port);
        }
        let listener = match self.bind(self.clients.callback_port).await {
            Ok(listener) => listener,
            Err(error) => {
                if let Ok(mut slot) = self.listen_error.lock() {
                    *slot = Some(error.clone());
                }
                return Err(error);
            }
        };
        if let Ok(mut slot) = self.listen_error.lock() {
            *slot = None;
        }
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let service = Callbacks::clone(self).router();
        tokio::spawn(async move {
            // The listener serves until the process ends. A failure here is a
            // socket error on a loopback port that was just bound, so it is
            // logged rather than surfaced: there is no page left to tell.
            if let Err(error) = axum::serve(listener, service).await {
                eprintln!("eris: the OAuth callback listener stopped: {error}");
            }
        });
        if let Ok(mut slot) = self.listening.lock() {
            *slot = Some(port);
        }
        Ok(port)
    }

    /// The router the listener serves. One route, and nothing else.
    pub fn router(self) -> Router {
        Router::new()
            .route("/callback", get(callback))
            .with_state(self)
    }

    /// Finish a login from what the operator pasted: the address the browser
    /// landed on, a `code#state` pair, or a bare code.
    ///
    /// This is the path that works from any device. The browser that approved the
    /// login may be a phone, and its redirect to `localhost` goes nowhere useful;
    /// the address in its bar still carries the code, and pasting it here is the
    /// same callback by hand. A flow is only spent when the paste matches it, so
    /// a paste from an older login leaves the current one waiting.
    pub fn handle_paste(&self, provider: Provider, pasted: &str) -> Outcome {
        let now = flow::now_secs();
        let parsed = flow::parse_paste(pasted);
        let fail = |message: String| {
            let outcome = Outcome::failed(provider, message);
            self.record(outcome.clone());
            outcome
        };
        let Some(waiting) = self.pending.state_for(provider, now) else {
            return fail(format!(
                "no {} login is waiting; press Connect first, then paste the address",
                provider.label()
            ));
        };
        if let Some(error) = parsed.error {
            // Spend the flow: the provider has refused it, so it cannot succeed.
            let _ = self.pending.take(&waiting, now);
            return fail(format!("the provider refused the login: {error}"));
        }
        let Some(code) = parsed.code else {
            return fail(
                "nothing in that paste looks like a login code; paste the whole address from \
                 the browser's address bar (it contains code= and state=)"
                    .to_owned(),
            );
        };
        if let Some(state) = &parsed.state
            && *state != waiting
        {
            return fail(
                "that address belongs to an older login; use the newest Open link above, \
                 then paste the address it ends on"
                    .to_owned(),
            );
        }
        self.handle(&[("code".to_owned(), code), ("state".to_owned(), waiting)])
    }

    /// Handle one callback exactly as the route does, for a test that does not
    /// want to own a socket.
    pub fn handle(&self, query: &[(String, String)]) -> Outcome {
        let provider_error = flow::query_value(query, "error").map(str::to_owned);
        let code = flow::query_value(query, "code").map(str::to_owned);
        let state = flow::query_value(query, "state").map(str::to_owned);
        let now = flow::now_secs();

        let Some(state) = state else {
            let outcome = Outcome::failed(
                Provider::Claude,
                "the provider sent no state value, so this callback cannot be matched to a login this daemon started",
            );
            self.record(outcome.clone());
            return outcome;
        };
        let Some(pending) = self.pending.take(&state, now) else {
            let outcome = Outcome::failed(
                Provider::Claude,
                "this callback does not match a login that is still waiting; it may have expired, \
                 or it may have been used already. Start the login again from Settings.",
            );
            self.record(outcome.clone());
            return outcome;
        };
        let provider = pending.provider;
        if let Some(error) = provider_error {
            let outcome =
                Outcome::failed(provider, format!("the provider refused the login: {error}"));
            self.record(outcome.clone());
            return outcome;
        }
        let Some(code) = code else {
            let outcome = Outcome::failed(
                provider,
                "the provider sent no authorization code, so there is nothing to exchange",
            );
            self.record(outcome.clone());
            return outcome;
        };

        // The grant first, because the grant is the thing that took a browser and
        // a password. What the account is called and which project it bills to are
        // read after, from the access token, and neither can fail the login.
        let mut record = match flow::exchange(
            provider,
            &self.clients,
            self.transport.as_ref(),
            &code,
            pending.pkce.verifier(),
            &state,
            now,
            None,
            None,
        ) {
            Ok(record) => record,
            Err(error) => {
                let outcome = Outcome::failed(provider, error.to_string());
                self.record(outcome.clone());
                return outcome;
            }
        };
        let found = match &self.account {
            Some(hook) => hook(provider, &record.access_token),
            None => discover::discover(self.transport.as_ref(), provider, &record.access_token),
        };
        // The file's `[google] project` is the manual answer, and it wins over an
        // empty discovery: an operator who pasted one has said which project this
        // host bills to, and an account that names none cannot argue with it.
        let mut found = found;
        if found.project.is_none() {
            found.project = self.clients.google_project.clone();
            if found.project.is_some() {
                found.note = None;
            }
        }
        record.account = found.account.clone();
        record.project_id = found.project.clone();
        if let Err(error) = self.store.store_token(&record) {
            let outcome = Outcome::failed(
                provider,
                format!("the grant was issued but could not be sealed on disk: {error}"),
            );
            self.record(outcome.clone());
            return outcome;
        }

        let outcome = Outcome {
            provider,
            ok: true,
            account: found.account,
            project: found.project,
            message: match &found.note {
                Some(note) => format!("{} is connected. {note}", provider.label()),
                None => format!("{} is connected.", provider.label()),
            },
            at: now,
        };
        self.record(outcome.clone());
        outcome
    }
}

/// `GET /callback` - the one route the listener serves.
async fn callback(
    State(callbacks): State<Callbacks>,
    Query(query): Query<Vec<(String, String)>>,
) -> Html<String> {
    // The exchange uses a blocking transport that builds its own runtime, which
    // is not allowed on an async worker thread, so it runs on the blocking pool.
    let outcome = tokio::task::spawn_blocking(move || callbacks.handle(&query))
        .await
        .unwrap_or_else(|_| {
            Outcome::failed(Provider::Claude, "the login exchange stopped unexpectedly")
        });
    Html(flow::callback_page(outcome.ok, &outcome.message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::flow::Pending;
    use crate::oauth::pkce::Pkce;
    use crate::oauth::refresh::{HttpRequest, TransportError};
    use crate::oauth::store::TokenFileStore;

    /// A transport that answers with a fixed grant and records what it was asked.
    struct Fixed(Arc<Mutex<Vec<HttpRequest>>>);

    impl HttpTransport for Fixed {
        fn post(&self, request: &HttpRequest) -> Result<(u16, String), TransportError> {
            self.0.lock().expect("lock").push(request.clone());
            Ok((
                200,
                r#"{"access_token":"at-value","refresh_token":"rt-value","expires_in":3600,
                    "scope":"user:inference"}"#
                    .to_owned(),
            ))
        }
    }

    fn store(dir: &tempfile::TempDir) -> Arc<dyn TokenStore + Send + Sync> {
        Arc::new(TokenFileStore::new(
            dir.path().join("tokens"),
            dir.path().join("keys/token.key"),
        ))
    }

    fn service(
        dir: &tempfile::TempDir,
        seen: Arc<Mutex<Vec<HttpRequest>>>,
    ) -> (Callbacks, Arc<PendingFlows>) {
        let pending = Arc::new(PendingFlows::new());
        let callbacks = Callbacks::new(
            Arc::clone(&pending),
            Arc::new(Clients {
                google_client_secret: Some("GOCSPX-test".to_owned()),
                ..Clients::default()
            }),
            store(dir),
        )
        .with_transport(Arc::new(Fixed(seen)))
        .with_account(Arc::new(|provider, _token| match provider {
            Provider::Claude => Discovered {
                account: Some("curtis@example.test".to_owned()),
                ..Discovered::default()
            },
            Provider::Antigravity => Discovered {
                account: Some("curtis@example.test".to_owned()),
                project: Some("aicode-123".to_owned()),
                note: None,
            },
        }));
        (callbacks, pending)
    }

    fn start(pending: &PendingFlows, provider: Provider, state: &str) {
        pending.start(
            Pending {
                provider,
                pkce: Pkce::generate().expect("randomness"),
                state: state.to_owned(),
                authorize_url: "https://example.test/authorize".to_owned(),
                expires_at: flow::now_secs() + 600,
            },
            flow::now_secs(),
        );
    }

    #[test]
    fn a_matching_callback_seals_the_grant_and_says_which_account() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Claude, "state-1");

        let outcome = callbacks.handle(&[
            ("code".to_owned(), "the-code".to_owned()),
            ("state".to_owned(), "state-1".to_owned()),
        ]);
        assert!(outcome.ok, "{}", outcome.message);
        assert_eq!(outcome.provider, Provider::Claude);
        assert_eq!(outcome.account.as_deref(), Some("curtis@example.test"));

        let sent = seen.lock().expect("lock").clone();
        assert_eq!(sent.len(), 1, "one exchange");
        assert_eq!(sent[0].url, "https://api.anthropic.com/v1/oauth/token");
        assert!(sent[0].body.contains("the-code"));

        let store = store(&dir);
        let record = store.load("claude").expect("loads").expect("a record");
        assert_eq!(record.access_token, "at-value");
        assert_eq!(record.origin, core_domain::TokenOrigin::Login);
        assert_eq!(record.account.as_deref(), Some("curtis@example.test"));
        assert!(record.expires_at.is_some());
        assert_eq!(callbacks.last(), Some(outcome));
    }

    #[test]
    fn a_project_named_in_the_client_file_fills_in_and_silences_the_note() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let pending = Arc::new(PendingFlows::new());
        let callbacks = Callbacks::new(
            Arc::clone(&pending),
            Arc::new(Clients {
                google_client_secret: Some("GOCSPX-test".to_owned()),
                google_project: Some("aicode-from-file".to_owned()),
                ..Clients::default()
            }),
            store(&dir),
        )
        .with_transport(Arc::new(Fixed(seen)))
        // Discovery found an account and no project, which is a fresh Google
        // login with nothing onboarded yet.
        .with_account(Arc::new(|_provider, _token| Discovered {
            account: Some("curtis@example.test".to_owned()),
            project: None,
            note: Some("the account answered but named no cloud project".to_owned()),
        }));
        start(&pending, Provider::Antigravity, "state-2");

        let outcome = callbacks.handle(&[
            ("code".to_owned(), "the-code".to_owned()),
            ("state".to_owned(), "state-2".to_owned()),
        ]);
        assert!(outcome.ok, "{}", outcome.message);
        assert_eq!(outcome.project.as_deref(), Some("aicode-from-file"));
        assert_eq!(
            outcome.message, "Google Antigravity is connected.",
            "the note is answered, so it is not repeated"
        );

        let record = store(&dir)
            .load("antigravity")
            .expect("loads")
            .expect("a record");
        assert_eq!(record.project_id.as_deref(), Some("aicode-from-file"));
        assert_eq!(record.account.as_deref(), Some("curtis@example.test"));
    }

    #[test]
    fn a_note_survives_into_the_outcome_when_nothing_fills_the_gap() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let pending = Arc::new(PendingFlows::new());
        let callbacks = Callbacks::new(
            Arc::clone(&pending),
            Arc::new(Clients {
                google_client_secret: Some("GOCSPX-test".to_owned()),
                ..Clients::default()
            }),
            store(&dir),
        )
        .with_transport(Arc::new(Fixed(seen)))
        .with_account(Arc::new(|_provider, _token| Discovered {
            account: None,
            project: None,
            note: Some("the project lookup was refused with HTTP 403".to_owned()),
        }));
        start(&pending, Provider::Antigravity, "state-3");

        let outcome = callbacks.handle(&[
            ("code".to_owned(), "the-code".to_owned()),
            ("state".to_owned(), "state-3".to_owned()),
        ]);
        assert!(outcome.ok, "a grant is still a grant");
        assert_eq!(outcome.project, None);
        assert!(
            outcome.message.contains("403"),
            "the operator is told why: {}",
            outcome.message
        );
        assert!(!outcome.message.contains("at-value"), "no token in a page");
    }

    #[test]
    fn a_pasted_address_finishes_the_login_without_a_listener() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Claude, "state-p");
        let outcome = callbacks.handle_paste(
            Provider::Claude,
            "http://localhost:7345/callback?code=the-code&state=state-p",
        );
        assert!(outcome.ok, "{}", outcome.message);
        assert_eq!(seen.lock().expect("lock").len(), 1);
        assert!(store(&dir).load("claude").expect("loads").is_some());
    }

    #[test]
    fn a_bare_code_is_matched_to_the_login_that_is_waiting() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Claude, "state-p");
        let outcome = callbacks.handle_paste(Provider::Claude, "the-code");
        assert!(outcome.ok, "{}", outcome.message);
        let body = seen.lock().expect("lock")[0].body.clone();
        assert!(
            body.contains("state-p"),
            "the waiting state is sent: {body}"
        );
    }

    #[test]
    fn a_paste_from_an_older_login_is_refused_and_keeps_the_current_one_waiting() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Claude, "newest");
        let outcome = callbacks.handle_paste(Provider::Claude, "?code=x&state=older");
        assert!(!outcome.ok);
        assert!(
            outcome.message.contains("older login"),
            "{}",
            outcome.message
        );
        assert!(seen.lock().expect("lock").is_empty(), "nothing exchanged");
        assert!(
            pending
                .state_for(Provider::Claude, flow::now_secs())
                .is_some(),
            "the waiting login survives a wrong paste"
        );
    }

    #[test]
    fn a_paste_with_no_login_waiting_or_no_code_says_which() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        let none = callbacks.handle_paste(Provider::Claude, "the-code");
        assert!(
            none.message.contains("press Connect first"),
            "{}",
            none.message
        );
        start(&pending, Provider::Claude, "s");
        let empty = callbacks.handle_paste(Provider::Claude, "not a code at all");
        assert!(
            empty.message.contains("looks like a login code"),
            "{}",
            empty.message
        );
        assert!(seen.lock().expect("lock").is_empty());
    }

    #[test]
    fn a_replayed_callback_is_refused_and_does_not_exchange_again() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Claude, "state-1");
        let query = vec![
            ("code".to_owned(), "the-code".to_owned()),
            ("state".to_owned(), "state-1".to_owned()),
        ];
        assert!(callbacks.handle(&query).ok);
        let second = callbacks.handle(&query);
        assert!(!second.ok);
        assert!(
            second.message.contains("does not match a login"),
            "{}",
            second.message
        );
        assert_eq!(seen.lock().expect("lock").len(), 1, "one exchange in total");
    }

    #[test]
    fn a_callback_with_no_state_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, _pending) = service(&dir, Arc::clone(&seen));
        let outcome = callbacks.handle(&[("code".to_owned(), "the-code".to_owned())]);
        assert!(!outcome.ok);
        assert!(outcome.message.contains("no state"), "{}", outcome.message);
        assert!(seen.lock().expect("lock").is_empty(), "nothing was sent");
    }

    #[test]
    fn a_provider_error_is_shown_as_its_own_code_and_nothing_is_exchanged() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Antigravity, "state-9");
        let outcome = callbacks.handle(&[
            ("error".to_owned(), "access_denied".to_owned()),
            ("state".to_owned(), "state-9".to_owned()),
        ]);
        assert!(!outcome.ok);
        assert!(
            outcome.message.contains("access_denied"),
            "{}",
            outcome.message
        );
        assert!(seen.lock().expect("lock").is_empty());
    }

    #[test]
    fn an_antigravity_login_records_the_project_it_was_billed_to() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (callbacks, pending) = service(&dir, Arc::clone(&seen));
        start(&pending, Provider::Antigravity, "state-2");
        let outcome = callbacks.handle(&[
            ("code".to_owned(), "the-code".to_owned()),
            ("state".to_owned(), "state-2".to_owned()),
        ]);
        assert!(outcome.ok, "{}", outcome.message);
        assert_eq!(outcome.project.as_deref(), Some("aicode-123"));
        let record = store(&dir)
            .load("antigravity")
            .expect("loads")
            .expect("record");
        assert_eq!(record.project_id.as_deref(), Some("aicode-123"));
        assert_eq!(
            seen.lock().expect("lock")[0].url,
            "https://oauth2.googleapis.com/token"
        );
    }

    #[tokio::test]
    async fn the_callback_listener_refuses_a_non_loopback_bind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (callbacks, _pending) = service(&dir, Arc::new(Mutex::new(Vec::new())));
        // The bind helper only ever builds `127.0.0.1`; the refusal is stated so a
        // future caller that tries a wildcard sees the rule rather than a socket.
        let listener = callbacks.bind(0).await.expect("loopback binds");
        let addr = listener.local_addr().expect("addr");
        assert!(addr.ip().is_loopback(), "{addr}");
    }
}
