//! The admin listener's address list.
//!
//! The default is [`super::ADMIN_ADDR`], loopback, and it stays the default: an
//! operator who never writes a `[admin]` table gets exactly the daemon 1.0
//! shipped with. What changed is that the list is now data, not a constant:
//!
//! - `admin.listen` in `config.toml` holds one or more `ip:port` binds, and every
//!   one of them is served simultaneously;
//! - `serve --admin-listen <ip:port>` is repeatable and *adds* to that list, so a
//!   one-off exposure needs no config edit;
//! - an absent or empty list means exactly one listener, `127.0.0.1:7340`.
//!
//! Nothing is hardcoded to Tailscale any more. A tailnet address, a LAN address,
//! a public address and the wildcard are all literals an operator may ask for,
//! and each of them means the same thing: bind that interface. Putting a reverse
//! proxy, a Cloudflare tunnel or Tailscale Serve in front of the loopback
//! listener is the job of [`crate::web::proxy`], which decides whether the
//! forwarding headers on an arriving request may be believed.
//!
//! Two facts are still enforced here, because they are mistakes rather than
//! choices: an entry that is not a literal `ip:port` (a hostname, a URL) is a
//! config error naming the value, and the same address listed twice is a config
//! error, because the second bind would fail with the address already in use.

use std::net::SocketAddr;

use crate::admin::ADMIN_ADDR;
#[cfg(test)]
use crate::admin::AppState;

/// Why a listener list was refused. Both variants carry the offending value, so
/// the message an operator sees names what they typed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListenError {
    /// The entry is not a literal `ip:port`.
    #[error(
        "admin listener {value} is not a literal ip:port: a hostname, a URL and a bare address with no port are refused"
    )]
    Malformed {
        /// The literal as written.
        value: String,
    },
    /// The same address appears more than once.
    #[error("admin listener {value} is listed twice: each address may bind once")]
    Duplicate {
        /// The literal as written.
        value: String,
    },
}

/// Parse one `ip:port` literal. Any literal address is accepted: loopback, a LAN
/// address, a tailnet address, a public address, `0.0.0.0` and `::`.
///
/// # Errors
///
/// Returns [`ListenError::Malformed`] for anything that is not a literal
/// `ip:port`, which is what keeps a hostname and a URL out of a bind.
pub fn parse_listen_entry(raw: &str) -> Result<SocketAddr, ListenError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ListenError::Malformed {
            value: String::from("<empty>"),
        });
    }
    trimmed
        .parse::<SocketAddr>()
        .map_err(|_| ListenError::Malformed {
            value: trimmed.to_owned(),
        })
}

/// The configured list, in bind order. An empty list means the default.
///
/// # Errors
///
/// A malformed entry or a duplicate is a config error.
pub fn parse_listen_list(raw: &[String]) -> Result<Vec<SocketAddr>, ListenError> {
    if raw.is_empty() {
        let default = ADMIN_ADDR
            .parse()
            .expect("the default is a literal ip:port");
        return Ok(vec![default]);
    }
    let mut out: Vec<SocketAddr> = Vec::with_capacity(raw.len());
    for entry in raw {
        let addr = parse_listen_entry(entry)?;
        if out.contains(&addr) {
            return Err(ListenError::Duplicate {
                value: entry.trim().to_owned(),
            });
        }
        out.push(addr);
    }
    Ok(out)
}

/// The effective list: what `config.toml` says, plus every `--admin-listen` flag.
///
/// The flag adds; it never replaces and never moves the configured binds, so
/// `eris serve --admin-listen 0.0.0.0:7340` serves the config list *and* the
/// wildcard. A flag that names an address already configured adds nothing,
/// because binding one address twice is an "address in use" failure, not a
/// second listener.
///
/// # Errors
///
/// A malformed flag is a startup error naming the value.
pub fn resolve_listeners(
    configured: &[SocketAddr],
    flags: &[String],
) -> Result<Vec<SocketAddr>, ListenError> {
    let mut out = configured.to_vec();
    for flag in flags {
        let addr = parse_listen_entry(flag)?;
        if !out.contains(&addr) {
            out.push(addr);
        }
    }
    Ok(out)
}

/// The addresses that actually get a socket, in bind order.
///
/// A wildcard on a port already covers every address of its family on that port,
/// so binding the wildcard and a specific address of the same family and port
/// cannot both succeed: the kernel refuses the second one with "address already
/// in use". The wildcard is the superset the operator asked for, so the specific
/// bind is redundant rather than a mistake, and it is dropped here instead of
/// turning a documented config into a startup failure. Duplicates of the *same*
/// literal are still a config error, because that is a typo, not an overlap.
///
/// Returns the effective bind list and the entries that were covered.
#[must_use]
pub fn effective_binds(addrs: &[SocketAddr]) -> (Vec<SocketAddr>, Vec<SocketAddr>) {
    let mut out: Vec<SocketAddr> = Vec::with_capacity(addrs.len());
    let mut covered: Vec<SocketAddr> = Vec::new();
    for addr in addrs {
        let same_family = |planned: &SocketAddr| planned.is_ipv4() == addr.is_ipv4();
        if out.iter().any(|planned| {
            planned.port() == addr.port() && same_family(planned) && is_wildcard(*planned)
        }) {
            covered.push(*addr);
            continue;
        }
        if is_wildcard(*addr) {
            let mut removed = Vec::new();
            out.retain(|planned| {
                let redundant = planned.port() == addr.port() && same_family(planned);
                if redundant {
                    removed.push(*planned);
                }
                !redundant
            });
            covered.extend(removed);
        }
        out.push(*addr);
    }
    (out, covered)
}

/// True for `0.0.0.0` and `::`: every interface this host has, at once.
#[must_use]
pub fn is_wildcard(addr: SocketAddr) -> bool {
    addr.ip().is_unspecified()
}

/// True when `ip` is inside the Tailscale CGNAT range `100.64.0.0/10`.
///
/// Kept for reporting: the bind policy no longer special-cases the range, but a
/// listener on a tailnet address is worth naming in a log line and in the ADR.
#[must_use]
pub fn is_tailnet(ip: std::net::IpAddr) -> bool {
    /// The Tailscale CGNAT block, `100.64.0.0/10`, as a network and mask.
    const TAILNET_V4_NETWORK: u32 = 0x6440_0000;
    /// The prefix mask that goes with `TAILNET_V4_NETWORK`.
    const TAILNET_V4_MASK: u32 = 0xFFC0_0000;
    match ip {
        std::net::IpAddr::V4(v4) => (u32::from(v4) & TAILNET_V4_MASK) == TAILNET_V4_NETWORK,
        std::net::IpAddr::V6(_) => false,
    }
}

#[test]
fn the_default_list_is_loopback_only() {
    let listeners = parse_listen_list(&[]).expect("an empty list is the default");
    let default: SocketAddr = ADMIN_ADDR.parse().expect("a literal socket address");
    assert_eq!(listeners, vec![default]);
    assert!(listeners[0].ip().is_loopback());
    assert!(!is_wildcard(listeners[0]));
}

#[tokio::test]
async fn multiple_addresses_are_all_bound() {
    let wants = vec!["127.0.0.1:0".to_owned(), "0.0.0.0:0".to_owned()];
    let listeners = parse_listen_list(&wants).expect("two literal addresses");
    assert_eq!(listeners.len(), 2, "{listeners:?}");

    let dir = tempfile::tempdir().expect("a temporary state root");
    let db = crate::db::Db::open(dir.path().join("eris.db")).expect("a fresh database");
    let paths = crate::config::Paths::for_root(dir.path());
    let state = AppState::new(crate::onboarding::SetupEngine::new(db, paths), false);

    let mut bound = Vec::new();
    for addr in listeners {
        bound.push(
            tokio::net::TcpListener::bind(addr)
                .await
                .expect("every configured address binds"),
        );
    }
    assert_eq!(bound.len(), 2, "both listeners are bound");

    let mut addrs = Vec::new();
    let mut tasks = Vec::new();
    for listener in bound {
        addrs.push(listener.local_addr().expect("a local address"));
        tasks.push(tokio::spawn(crate::admin::serve_on(
            listener,
            state.clone(),
        )));
    }
    for addr in &addrs {
        let (status, body) = get_once(*addr, "/healthz").await;
        assert_eq!(status, 200, "{addr} did not answer: {body}");
        assert!(body.contains(r#""status":"ok""#), "{body}");
    }
    for task in tasks {
        task.abort();
    }
}

#[test]
fn a_wildcard_bind_subsumes_the_same_port_loopback() {
    // A live config: loopback first, then the wildcard on the same port.
    // The kernel refuses the second bind, and the wildcard already serves
    // loopback, so the wildcard is kept and the loopback entry is reported as
    // covered rather than as an error.
    let wants = vec!["127.0.0.1:7340".to_owned(), "0.0.0.0:7340".to_owned()];
    let configured = parse_listen_list(&wants).expect("two distinct literals, not a duplicate");
    let (effective, covered) = effective_binds(&configured);
    assert_eq!(
        effective,
        vec!["0.0.0.0:7340".parse::<SocketAddr>().expect("literal")]
    );
    assert_eq!(
        covered,
        vec!["127.0.0.1:7340".parse::<SocketAddr>().expect("literal")]
    );

    // The other order is the same answer.
    let reversed = vec!["0.0.0.0:7340".to_owned(), "127.0.0.1:7340".to_owned()];
    let (effective, covered) = effective_binds(&parse_listen_list(&reversed).expect("two binds"));
    assert_eq!(
        effective,
        vec!["0.0.0.0:7340".parse::<SocketAddr>().expect("literal")]
    );
    assert_eq!(covered.len(), 1);

    // Two specific addresses on one port are not redundant with each other.
    let specific = vec!["127.0.0.1:7340".to_owned(), "192.168.1.10:7340".to_owned()];
    let (effective, covered) =
        effective_binds(&parse_listen_list(&specific).expect("two specific binds"));
    assert_eq!(effective.len(), 2);
    assert!(covered.is_empty());
}

#[test]
fn a_wildcard_address_is_accepted_when_asked_for() {
    let wants = vec!["0.0.0.0:7340".to_owned(), "[::]:7340".to_owned()];
    let listeners = parse_listen_list(&wants).expect("a wildcard is bound when it is asked for");
    assert_eq!(listeners.len(), 2);
    for addr in &listeners {
        assert!(addr.ip().is_unspecified(), "{addr} is the wildcard address");
        assert!(is_wildcard(*addr), "{addr} is reported as a wildcard");
    }
}

#[test]
fn a_duplicate_address_is_a_config_error() {
    let wants = vec!["127.0.0.1:7340".to_owned(), "127.0.0.1:7340".to_owned()];
    let error = parse_listen_list(&wants).expect_err("a duplicate is refused");
    let message = error.to_string();
    assert!(message.contains("127.0.0.1:7340"), "{message}");
    assert!(
        message.to_ascii_lowercase().contains("twice")
            || message.to_ascii_lowercase().contains("duplicate"),
        "{message}"
    );
}

#[test]
fn a_malformed_address_is_a_config_error() {
    for raw in [
        "localhost:7340",
        "daemon.example.ts.net:7340",
        "127.0.0.1",
        "0.0.0.0",
        "https://kallisti.example.com",
        "not an address",
    ] {
        let error = parse_listen_list(&[raw.to_owned()]).expect_err("a malformed entry is refused");
        assert!(
            error.to_string().contains(raw),
            "the error names the bad value {raw}: {error}"
        );
    }
}

#[test]
fn the_flag_adds_to_the_config_list() {
    let configured = parse_listen_list(&["127.0.0.1:7340".to_owned()]).expect("the default");
    let with_flag = resolve_listeners(&configured, &["0.0.0.0:7340".to_owned()])
        .expect("the flag adds to the configured list");
    assert_eq!(
        with_flag,
        vec![
            "127.0.0.1:7340".parse().expect("literal"),
            "0.0.0.0:7340".parse().expect("literal"),
        ]
    );
    assert_eq!(
        with_flag[0], configured[0],
        "the configured bind stays first"
    );

    // Naming an address that is already configured adds nothing: binding it
    // twice would fail with the address already in use.
    let same = resolve_listeners(&configured, &["127.0.0.1:7340".to_owned()])
        .expect("naming the configured address is not an error");
    assert_eq!(same, configured);

    // A malformed flag is still a startup error that names the value.
    let refused = resolve_listeners(&configured, &["localhost:7340".to_owned()])
        .expect_err("a hostname flag is refused");
    assert!(refused.to_string().contains("localhost:7340"), "{refused}");
}

#[test]
fn a_hostname_is_refused() {
    for raw in [
        "localhost:7340",
        "daemon.example.ts.net:7340",
        "example.com:7340",
    ] {
        let error = parse_listen_entry(raw).expect_err("a hostname is refused");
        assert!(
            error.to_string().contains("not a literal ip:port"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn both_listeners_serve_the_same_healthz() {
    let dir = tempfile::tempdir().expect("a temporary state root");
    let db = crate::db::Db::open(dir.path().join("eris.db")).expect("a fresh database");
    let paths = crate::config::Paths::for_root(dir.path());
    let state = AppState::new(crate::onboarding::SetupEngine::new(db, paths), false);

    // Two listeners, one `AppState`, one router: the shape `serve_listeners`
    // builds. Ephemeral ports keep the test independent of whatever else is on
    // this host.
    let first = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback listener");
    let first_addr = first.local_addr().expect("a local address");
    let second = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback listener");
    let second_addr = second.local_addr().expect("a local address");
    assert_ne!(first_addr, second_addr);

    let alpha = tokio::spawn(crate::admin::serve_on(first, state.clone()));
    let beta = tokio::spawn(crate::admin::serve_on(second, state));

    let (status_a, body_a) = get_once(first_addr, "/healthz").await;
    let (status_b, body_b) = get_once(second_addr, "/healthz").await;

    assert_eq!(status_a, 200);
    assert_eq!(status_b, 200);
    assert_eq!(body_a, body_b, "both listeners answer the same healthz");
    assert!(body_a.contains(r#""status":"ok""#), "{body_a}");

    alpha.abort();
    beta.abort();
}

/// One HTTP/1.1 GET, read to EOF, with a minimal chunk decoder.
#[cfg(test)]
async fn get_once(addr: SocketAddr, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to the listener");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write the request");
    let mut raw = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut raw),
    )
    .await
    .expect("the response arrives within the timeout")
    .expect("read the response");

    let text = String::from_utf8(raw).expect("a utf-8 response");
    let (head, body) = text.split_once("\r\n\r\n").expect("a header block");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    (
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_owned()
        },
    )
}

/// Minimal chunk decoder: hyper usually sends `content-length`, but the test
/// must not depend on that.
#[cfg(test)]
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, remainder)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        if remainder.len() < size {
            out.push_str(remainder);
            break;
        }
        out.push_str(&remainder[..size]);
        rest = remainder[size..]
            .strip_prefix("\r\n")
            .unwrap_or(&remainder[size..]);
    }
    out
}
