//! Reverse proxies in front of the admin listener: who is allowed to say who
//! the client is.
//!
//! The rule is one sentence long: forwarding headers are believed only when the
//! immediate TCP peer is inside `admin.trusted_proxies`. A peer that is not
//! trusted and sends them is attempting to spoof, and that is still the 403
//! `not_loopback` it was before this module existed.
//!
//! Why this replaced an outright rejection of every forwarded header: a proxy in
//! front of a loopback daemon is the normal way to reach it from a phone, a
//! laptop on the LAN, a tailnet, or the internet through a Cloudflare tunnel.
//! Rejecting the headers made every one of those paths impossible, and did it
//! whether or not the operator had asked for a proxy. Trusting the *peer* rather
//! than banning the *header* keeps the spoofing defence (a remote client cannot
//! make its packet arrive from 127.0.0.1) while letting the proxy do its job.
//!
//! The default set is loopback only, which is what Tailscale Serve, a local
//! nginx and a local Caddy all are. Adding a LAN proxy is one config line
//! (`admin.trusted_proxies`), and the consequence is stated in the ADR.
//!
//! Everything derived here lands in [`ClientContext`] as a request extension, so
//! a handler, the audit trail and the rate limiter read the real client instead
//! of the proxy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::admin::{AppState, forwarded_header};
use crate::error::{ApiError, RequestId};

/// How many `X-Forwarded-For` hops are read. Only the first (the client) and the
/// last (the nearest proxy) matter; a chain longer than this is not something a
/// proxy this daemon trusts produces.
const MAX_XFF_HOPS: usize = 32;

/// A parsed IPv4 or IPv6 network, small enough that no new crate is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    base: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse `addr/prefix` or a bare address. A bare address is a single host:
    /// `/32` for IPv4, `/128` for IPv6.
    ///
    /// # Errors
    ///
    /// Names the value and the reason when it does not parse.
    pub fn parse(raw: &str) -> Result<Self, ProxyConfigError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ProxyConfigError::bad_cidr(raw, "the value is empty"));
        }
        let (addr_text, prefix_text) = match trimmed.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (trimmed, None),
        };
        let base: IpAddr = addr_text.parse().map_err(|_| {
            ProxyConfigError::bad_cidr(raw, "the address half is not an IP address")
        })?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix_text {
            None => max,
            Some(text) => {
                let value: u8 = text
                    .parse()
                    .map_err(|_| ProxyConfigError::bad_cidr(raw, "the prefix is not a number"))?;
                if value > max {
                    return Err(ProxyConfigError::bad_cidr(
                        raw,
                        format!("the prefix is larger than {max}"),
                    ));
                }
                value
            }
        };
        Ok(Self { base, prefix })
    }

    /// True when `ip` is inside this network. A v6-mapped v4 address is compared
    /// as the v4 address it carries, so `127.0.0.1/32` matches `::ffff:127.0.0.1`.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        match (self.base, ip) {
            (IpAddr::V4(base), IpAddr::V4(ip)) => {
                let mask = mask_v4(self.prefix);
                (u32::from(base) & mask) == (u32::from(ip) & mask)
            }
            (IpAddr::V6(base), IpAddr::V6(ip)) => {
                let mask = mask_v6(self.prefix);
                (u128::from(base) & mask) == (u128::from(ip) & mask)
            }
            _ => false,
        }
    }
}

/// Mask for an IPv4 prefix length.
fn mask_v4(prefix: u8) -> u32 {
    match prefix {
        0 => 0,
        32..=u8::MAX => u32::MAX,
        other => u32::MAX << (32 - other),
    }
}

/// Mask for an IPv6 prefix length.
fn mask_v6(prefix: u8) -> u128 {
    match prefix {
        0 => 0,
        128..=u8::MAX => u128::MAX,
        other => u128::MAX << (128 - other),
    }
}

/// Unwrap a v4-mapped IPv6 address to the v4 address it carries.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    }
}

/// Why a proxy setting was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyConfigError {
    /// A `trusted_proxies` entry is not an IP or a CIDR.
    #[error("admin.trusted_proxies entry {value} is not an IP address or CIDR: {reason}")]
    BadCidr {
        /// The entry as written.
        value: String,
        /// Why it was refused.
        reason: String,
    },
    /// `public_url` is not a usable absolute http(s) URL.
    #[error("admin.public_url {value} is not a usable http(s) URL: {reason}")]
    BadPublicUrl {
        /// The value as written.
        value: String,
        /// Why it was refused.
        reason: String,
    },
}

impl ProxyConfigError {
    fn bad_cidr(value: &str, reason: impl Into<String>) -> Self {
        Self::BadCidr {
            value: value.to_owned(),
            reason: reason.into(),
        }
    }

    fn bad_url(value: &str, reason: impl Into<String>) -> Self {
        Self::BadPublicUrl {
            value: value.to_owned(),
            reason: reason.into(),
        }
    }
}

/// The peers whose forwarding headers this daemon believes.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies {
    nets: Vec<Cidr>,
}

impl TrustedProxies {
    /// Loopback only: what a daemon on this host needs to sit behind Tailscale
    /// Serve, a local nginx, or a local Caddy without any config at all.
    #[must_use]
    pub fn default_loopback() -> Self {
        let nets = vec![
            Cidr {
                base: IpAddr::V4(Ipv4Addr::LOCALHOST),
                prefix: 32,
            },
            Cidr {
                base: IpAddr::V6(Ipv6Addr::LOCALHOST),
                prefix: 128,
            },
        ];
        Self { nets }
    }

    /// Parse `admin.trusted_proxies`. An empty list means loopback only, and a
    /// duplicate network is harmless, so it is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ProxyConfigError::BadCidr`] naming the first bad entry.
    pub fn parse(raw: &[String]) -> Result<Self, ProxyConfigError> {
        if raw.is_empty() {
            return Ok(Self::default_loopback());
        }
        let mut nets = Vec::with_capacity(raw.len());
        for entry in raw {
            nets.push(Cidr::parse(entry)?);
        }
        Ok(Self { nets })
    }

    /// True when `ip` is a trusted peer.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.nets.iter().any(|net| net.contains(ip))
    }

    /// How many networks are trusted. Loopback only is two.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nets.len()
    }

    /// True when nothing is trusted, which is only reachable by constructing the
    /// type directly: the default is loopback.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }
}

/// Everything the listener learned about access from the config: who may set
/// forwarding headers, and what the operator calls this daemon in public.
#[derive(Debug, Clone, Default)]
pub struct AdminAccess {
    /// Peers allowed to set forwarding headers.
    pub trusted: TrustedProxies,
    /// `admin.public_url`, when one is configured.
    pub public_url: Option<String>,
}

impl AdminAccess {
    /// Default: loopback proxies, no public URL. This is the out-of-the-box
    /// daemon.
    #[must_use]
    pub fn loopback_only() -> Self {
        Self {
            trusted: TrustedProxies::default_loopback(),
            public_url: None,
        }
    }

    /// Parse both settings from the config file.
    ///
    /// # Errors
    ///
    /// A bad CIDR or a bad URL is a config error naming the value.
    pub fn parse(
        trusted_proxies: &[String],
        public_url: Option<&str>,
    ) -> Result<Self, ProxyConfigError> {
        Ok(Self {
            trusted: TrustedProxies::parse(trusted_proxies)?,
            public_url: match public_url {
                None => None,
                Some(raw) => Some(validate_public_url(raw)?),
            },
        })
    }

    /// A value for tests and for the embedding harness.
    #[must_use]
    pub fn for_test(public_url: Option<&str>, trusted: TrustedProxies) -> Self {
        Self {
            trusted,
            public_url: public_url.map(str::to_owned),
        }
    }

    /// Whether the session cookie carries `Secure`.
    ///
    /// True when the operator configured an `https` public URL, or when the
    /// request itself arrived over a trusted `https` proxy. A `Secure` cookie on
    /// a plain loopback page would never be stored by the browser, so the answer
    /// is never simply "always".
    #[must_use]
    pub fn cookie_secure(&self, context: &ClientContext) -> bool {
        if self
            .public_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://"))
        {
            return true;
        }
        context.scheme == "https"
    }
}

/// Validate `admin.public_url`: absolute `http` or `https`, a host, no query and
/// no fragment. It is used for absolute links and for the `Secure` decision, so
/// it must not be something a browser would resolve differently.
fn validate_public_url(raw: &str) -> Result<String, ProxyConfigError> {
    let trimmed = raw.trim();
    let lowered = trimmed.to_ascii_lowercase();
    let rest = if let Some(rest) = lowered.strip_prefix("https://") {
        rest
    } else if let Some(rest) = lowered.strip_prefix("http://") {
        rest
    } else {
        return Err(ProxyConfigError::bad_url(
            raw,
            "it must start with http:// or https://",
        ));
    };
    if trimmed.contains('?') {
        return Err(ProxyConfigError::bad_url(
            raw,
            "it must not carry a query string",
        ));
    }
    if trimmed.contains('#') {
        return Err(ProxyConfigError::bad_url(
            raw,
            "it must not carry a fragment",
        ));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err(ProxyConfigError::bad_url(raw, "it must name a host"));
    }
    // Strip the trailing slash so every link builder appends the same way.
    let trimmed = trimmed.trim_end_matches('/');
    Ok(trimmed.to_owned())
}

/// The client as this request should be understood: who it really is, over what
/// scheme it is talking, and under which host name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientContext {
    /// The TCP peer, when the transport reported one.
    pub peer: Option<SocketAddr>,
    /// Whether that peer is a trusted proxy.
    pub trusted: bool,
    /// The effective client: the forwarded address when the peer is trusted,
    /// otherwise the peer itself.
    pub client_ip: Option<IpAddr>,
    /// `http` or `https`.
    pub scheme: &'static str,
    /// The host the client asked for, forwarded or not.
    pub host: Option<String>,
}

impl Default for ClientContext {
    /// No peer, no proxy, plain HTTP: what a handler sees when it is driven
    /// outside the middleware.
    fn default() -> Self {
        Self {
            peer: None,
            trusted: false,
            client_ip: None,
            scheme: "http",
            host: None,
        }
    }
}

impl ClientContext {
    /// Build the context from the peer and the request headers.
    #[must_use]
    pub fn derive(peer: Option<SocketAddr>, headers: &HeaderMap, trusted: &TrustedProxies) -> Self {
        let trusted = peer.is_some_and(|addr| trusted.contains(addr.ip()));
        let host = host_header(headers);
        if !trusted {
            return Self {
                peer,
                trusted: false,
                client_ip: peer.map(|addr| addr.ip()),
                scheme: "http",
                host,
            };
        }
        let client_ip = forwarded_for(headers)
            .or_else(|| header_ip(headers, "x-real-ip"))
            .or(peer.map(|addr| addr.ip()));
        let scheme = match header_str(headers, "x-forwarded-proto") {
            Some(value) if value.eq_ignore_ascii_case("https") => "https",
            Some(value) if value.eq_ignore_ascii_case("http") => "http",
            // Anything else, including a missing header, is the connection's own
            // scheme, which for this daemon is plain HTTP.
            _ => "http",
        };
        let host = header_str(headers, "x-forwarded-host")
            .or_else(|| header_str(headers, "x-forwarded-server"))
            .or(host);
        Self {
            peer,
            trusted: true,
            client_ip,
            scheme,
            host,
        }
    }

    /// The audit actor for this request: the effective client address, or
    /// `loopback` when there is none. Never a secret, never the proxy.
    #[must_use]
    pub fn actor(&self) -> String {
        match self.client_ip {
            Some(ip) => ip.to_string(),
            None => String::from("loopback"),
        }
    }

    /// The scheme the client sees. `https` is what makes the cookie secure.
    #[must_use]
    pub fn scheme(&self) -> &'static str {
        self.scheme
    }
}

/// The `Host` header, trimmed.
fn host_header(headers: &HeaderMap) -> Option<String> {
    header_str(headers, "host")
}

/// One header as a trimmed string.
fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').next().unwrap_or(value).trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// One header parsed as an IP address.
fn header_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    header_str(headers, name).and_then(|value| {
        value
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .ok()
    })
}

/// The first usable hop of `X-Forwarded-For`.
///
/// The first entry is the original client, which is what the audit trail and the
/// rate limiter want. A malformed entry is skipped rather than trusted as text.
#[must_use]
pub fn forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    let value = headers.get("x-forwarded-for")?.to_str().ok()?;
    value
        .split(',')
        .take(MAX_XFF_HOPS)
        .map(str::trim)
        .find_map(|hop| hop.parse::<IpAddr>().ok())
}

/// The middleware: decide the client, or refuse a spoofing attempt.
///
/// A request carrying a forwarding header from a peer that is not trusted is a
/// 403 `not_loopback`, exactly as it was when every forwarded header was
/// refused. Everything else gets a [`ClientContext`] in its request extensions
/// and continues.
pub async fn client_context_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let context = ClientContext::derive(peer, request.headers(), &state.access.trusted);

    if !context.trusted
        && let Some(name) = forwarded_header(request.headers())
    {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map(|id| id.0.clone())
            .unwrap_or_default();
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "not_loopback",
            format!(
                "the admin listener answers a direct client or a trusted proxy; this request arrived from {} carrying the {name} header, which is not a trusted proxy",
                context
                    .peer
                    .map(|addr| addr.ip().to_string())
                    .unwrap_or_else(|| String::from("an unknown peer"))
            ),
            request_id,
        )
        .into_response();
    }

    request.extensions_mut().insert(context);
    next.run(request).await
}

#[cfg(test)]
use crate::admin::{ROUTE_HEALTHZ, ROUTE_LOGIN, router_with};
#[cfg(test)]
use crate::web::harness::{Call, block_on, encode, ready_engine, send};
#[cfg(test)]
use axum::http::header;

#[test]
fn a_bad_cidr_is_a_config_error() {
    for raw in ["10.0.0.0/33", "192.168.1.0/nope", "300.1.1.1/24"] {
        let error =
            TrustedProxies::parse(&[raw.to_owned()]).expect_err("a bad CIDR is a config error");
        assert!(
            error.to_string().contains(raw),
            "the error names the bad value {raw}: {error}"
        );
    }
    let empty = TrustedProxies::parse(&[String::new()]).expect_err("an empty entry is refused");
    assert!(
        empty.to_string().contains("not an IP address or CIDR"),
        "{empty}"
    );

    // A bare address is a single host, IPv4 and IPv6 alike.
    let hosts = TrustedProxies::parse(&[
        "192.168.1.5".to_owned(),
        "fd00::1".to_owned(),
        "10.0.0.0/8".to_owned(),
        "fd00::/8".to_owned(),
    ])
    .expect("bare addresses and CIDRs both parse");
    assert!(hosts.contains("192.168.1.5".parse().expect("ip")));
    assert!(!hosts.contains("192.168.1.6".parse().expect("ip")));
    assert!(hosts.contains("10.1.2.3".parse().expect("ip")));
    assert!(hosts.contains("fd00::1".parse().expect("ip")));
    assert!(hosts.contains("fd00::99".parse().expect("ip")));
    assert!(!hosts.contains("192.0.2.1".parse().expect("ip")));

    // The default is loopback, both families, and nothing else.
    let default = TrustedProxies::default_loopback();
    assert!(default.contains("127.0.0.1".parse().expect("ip")));
    assert!(default.contains("::1".parse().expect("ip")));
    assert!(!default.contains("127.0.0.2".parse().expect("ip")));
    assert!(!default.contains("192.168.1.5".parse().expect("ip")));
}

#[test]
fn a_trusted_peer_may_send_forwarded_headers() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false).with_access(AdminAccess::loopback_only()));

    let trusted = SocketAddr::from(([127, 0, 0, 1], 54321));
    let reply = block_on(send(
        &app,
        Call::get(ROUTE_HEALTHZ)
            .peer(trusted)
            .header("x-forwarded-for", "203.0.113.9")
            .header("x-forwarded-proto", "https"),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

    // A configured LAN proxy is trusted too, once it is in the list.
    let access =
        AdminAccess::parse(&["192.168.1.0/24".to_owned()], None).expect("a LAN CIDR parses");
    let (_lan_dir, lan_engine, _lan_passphrase) = ready_engine();
    let lan_app = router_with(AppState::new(lan_engine, false).with_access(access));
    let reply = block_on(send(
        &lan_app,
        Call::get(ROUTE_HEALTHZ)
            .peer(SocketAddr::from(([192, 168, 1, 20], 4000)))
            .header("x-forwarded-for", "203.0.113.9"),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

#[test]
fn an_untrusted_peer_sending_forwarded_headers_is_403() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false).with_access(AdminAccess::loopback_only()));

    // A LAN peer, a public peer, and a request with no peer information at all:
    // all three are untrusted, so the forwarded header is a spoof attempt.
    for peer in [
        SocketAddr::from(([192, 168, 1, 9], 4444)),
        SocketAddr::from(([203, 0, 113, 9], 4444)),
    ] {
        let reply = block_on(send(
            &app,
            Call::get(ROUTE_HEALTHZ)
                .peer(peer)
                .header("x-forwarded-for", "203.0.113.9"),
        ));
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
        assert_eq!(reply.json()["code"], "not_loopback");
    }

    let unknown = block_on(send(
        &app,
        Call::get(ROUTE_HEALTHZ).header("x-forwarded-host", "kallisti.example.com"),
    ));
    assert_eq!(unknown.status, StatusCode::FORBIDDEN, "{}", unknown.body);
    assert_eq!(unknown.json()["code"], "not_loopback");

    // The same untrusted peer is served normally with no forwarding header: the
    // refusal is about the spoof, not about who is asking.
    let plain = block_on(send(
        &app,
        Call::get(ROUTE_HEALTHZ).peer(SocketAddr::from(([192, 168, 1, 9], 4444))),
    ));
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.body);
}

#[test]
fn the_client_context_reports_the_forwarded_address_and_scheme() {
    let trusted = TrustedProxies::default_loopback();
    let peer = SocketAddr::from(([127, 0, 0, 1], 5555));

    let mut headers = HeaderMap::new();
    headers.insert(
        header::HeaderName::from_static("x-forwarded-for"),
        "203.0.113.9, 10.0.0.7".parse().expect("a header value"),
    );
    headers.insert(
        header::HeaderName::from_static("x-forwarded-proto"),
        "https".parse().expect("a header value"),
    );
    headers.insert(
        header::HeaderName::from_static("x-forwarded-host"),
        "kallisti.example.com".parse().expect("a header value"),
    );

    let context = ClientContext::derive(Some(peer), &headers, &trusted);
    assert!(context.trusted, "loopback is a trusted proxy");
    assert_eq!(
        context.client_ip,
        Some("203.0.113.9".parse::<IpAddr>().expect("ip")),
        "the first hop is the client, not the last proxy"
    );
    assert_eq!(context.scheme, "https", "the forwarded scheme is adopted");
    assert_eq!(
        context.host.as_deref(),
        Some("kallisti.example.com"),
        "the forwarded host is adopted"
    );
    assert_eq!(context.actor(), "203.0.113.9", "the actor is the client");

    // Without a forwarding header the peer is the client and the connection
    // scheme is the scheme.
    let direct = ClientContext::derive(Some(peer), &HeaderMap::new(), &trusted);
    assert_eq!(
        direct.client_ip,
        Some(IpAddr::from([127, 0, 0, 1])),
        "a direct peer is its own client"
    );
    assert_eq!(direct.scheme, "http");
    assert_eq!(direct.actor(), "127.0.0.1");

    // X-Real-IP is the second source, after X-Forwarded-For.
    let mut real_ip = HeaderMap::new();
    real_ip.insert(
        header::HeaderName::from_static("x-real-ip"),
        "198.51.100.8".parse().expect("a header value"),
    );
    let context = ClientContext::derive(Some(peer), &real_ip, &trusted);
    assert_eq!(
        context.client_ip,
        Some("198.51.100.8".parse().expect("ip")),
        "X-Real-IP is honoured when X-Forwarded-For is absent"
    );

    // A peer outside the trusted set never gets to name the client.
    let context = ClientContext::derive(
        Some(SocketAddr::from(([198, 51, 100, 4], 9999))),
        &headers,
        &trusted,
    );
    assert!(!context.trusted);
    assert_eq!(
        context.actor(),
        "198.51.100.4",
        "an untrusted peer is the client, whatever it claims"
    );
}

#[test]
fn an_https_public_url_makes_the_cookie_secure() {
    let (_dir, engine, passphrase) = ready_engine();
    let access = AdminAccess::for_test(
        Some("https://kallisti.example.com"),
        TrustedProxies::default_loopback(),
    );
    let app = router_with(AppState::new(engine.clone(), false).with_access(access));

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_LOGIN, &format!("passphrase={}", encode(&passphrase)))
            .peer(SocketAddr::from(([127, 0, 0, 1], 5555))),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let cookie = reply
        .headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("a session cookie");
    assert!(
        cookie.contains("Secure"),
        "an https public URL makes the cookie Secure: {cookie}"
    );
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");

    // The same daemon with no public URL and a plain HTTP peer stays insecure,
    // exactly as it was: a Secure cookie over loopback HTTP never comes back.
    let (_plain_dir, plain_engine, plain_passphrase) = ready_engine();
    let plain = router_with(AppState::new(plain_engine, false));
    let reply = block_on(send(
        &plain,
        Call::post(
            ROUTE_LOGIN,
            &format!("passphrase={}", encode(&plain_passphrase)),
        ),
    ));
    let cookie = reply
        .headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("a session cookie");
    assert!(
        !cookie.contains("Secure"),
        "a loopback HTTP page must not set Secure: {cookie}"
    );
}

#[test]
fn a_public_url_must_be_an_absolute_http_url() {
    for raw in [
        "kallisti.example.com",
        "ftp://kallisti.example.com",
        "https://kallisti.example.com/path?x=1",
        "https://kallisti.example.com/#frag",
        "https://",
    ] {
        let error = AdminAccess::parse(&[], Some(raw)).expect_err("a bad public URL is refused");
        assert!(
            matches!(error, ProxyConfigError::BadPublicUrl { .. }),
            "{raw}: {error}"
        );
    }
    let access = AdminAccess::parse(&[], Some("https://kallisti.example.com/"))
        .expect("a plain https URL is accepted");
    assert_eq!(
        access.public_url.as_deref(),
        Some("https://kallisti.example.com"),
        "a trailing slash is normalised away"
    );
    assert!(
        AdminAccess::parse(&[], None)
            .expect("no public URL is fine")
            .public_url
            .is_none()
    );
}
