//! The admin listener's address policy: loopback by default, plus one opt-in
//! second listener on this host's own tailnet address.
//!
//! [`super::ADMIN_ADDR`] stays the default and stays loopback. This module is the
//! only place a second admin bind can come from, and it is narrow on purpose:
//!
//! - a loopback address (`127.0.0.0/8` or `::1`) is accepted;
//! - an IPv4 address in the Tailscale CGNAT range `100.64.0.0/10` is accepted
//!   only when it is actually assigned to an interface on this host;
//! - everything else is refused before anything binds: `0.0.0.0`, `::`, a private
//!   LAN address, a public address, a bare address with no port, and any hostname.
//!
//! The local-assignment check is a `UdpSocket` connect, not a subprocess: the
//! kernel picks a route and a source address for the destination, and if it picks
//! the address that was asked about, that address is ours. A UDP connect sends
//! nothing, and the daemon never shells out to `tailscale` or `ip`.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

#[cfg(test)]
use crate::admin::{ADMIN_ADDR, AppState};

/// The Tailscale CGNAT block, `100.64.0.0/10`, as a network and mask.
const TAILNET_V4_NETWORK: u32 = 0x6440_0000;
/// The prefix mask that goes with [`TAILNET_V4_NETWORK`].
const TAILNET_V4_MASK: u32 = 0xFFC0_0000;

/// An address that `--admin-listen` will not bind, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "refusing --admin-listen {addr}: the admin UI may bind only loopback or this host's tailnet \
     address ({reason})"
)]
pub struct ListenRefused {
    /// The literal the operator passed, exactly as it was passed.
    pub addr: String,
    /// Why it was refused, in plain words.
    pub reason: String,
}

impl ListenRefused {
    fn new(addr: &str, reason: impl Into<String>) -> Self {
        Self {
            addr: addr.to_owned(),
            reason: reason.into(),
        }
    }
}

/// What an address is, for the bind policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `127.0.0.0/8` or `::1`.
    Loopback,
    /// `100.64.0.0/10`.
    Tailnet,
    /// `0.0.0.0` or `::`.
    Wildcard,
    /// `10/8`, `172.16/12`, `192.168/16` or `169.254/16`.
    Lan,
    /// Any other IPv4 address.
    Public,
    /// An IPv6 address that is neither loopback nor unspecified. Tailscale's
    /// IPv6 block is not part of this policy.
    NotTailnetV6,
}

/// True when `ip` is inside the Tailscale CGNAT range `100.64.0.0/10`.
#[must_use]
pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => (u32::from(v4) & TAILNET_V4_MASK) == TAILNET_V4_NETWORK,
        IpAddr::V6(_) => false,
    }
}

/// True when `ip` is assigned to an interface on this host.
///
/// A `UdpSocket` connect asks the kernel for a route and a source address without
/// sending a packet. If the kernel's chosen source is the address we asked about,
/// the address is ours. There is no `getifaddrs` binding in `std`, and a
/// subprocess would put `tailscale` or `ip` in the daemon's trust boundary.
#[must_use]
pub fn is_locally_assigned(ip: IpAddr) -> bool {
    let wildcard = match ip {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let Ok(socket) = UdpSocket::bind(wildcard) else {
        return false;
    };
    if socket.connect(SocketAddr::new(ip, 9)).is_err() {
        return false;
    }
    socket
        .local_addr()
        .map(|local| local.ip() == ip)
        .unwrap_or(false)
}

/// Classify one address for the bind policy.
fn classify(ip: IpAddr) -> Class {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                Class::Loopback
            } else if v4.is_unspecified() {
                Class::Wildcard
            } else if is_tailnet(IpAddr::V4(v4)) {
                Class::Tailnet
            } else if is_private_v4(v4) {
                Class::Lan
            } else {
                Class::Public
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                Class::Loopback
            } else if v6.is_unspecified() {
                Class::Wildcard
            } else {
                Class::NotTailnetV6
            }
        }
    }
}

/// The RFC 1918 blocks, plus link-local, which is what a LAN hands out.
fn is_private_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 169 && b == 254)
}

/// Validate one `--admin-listen` value against this host.
///
/// # Errors
///
/// Returns [`ListenRefused`] for anything that is not a loopback address or this
/// host's own tailnet address.
pub fn parse_admin_listen(raw: &str) -> Result<SocketAddr, ListenRefused> {
    parse_admin_listen_with(raw, &is_locally_assigned)
}

/// [`parse_admin_listen`] with the local-assignment check injected, so the policy
/// can be tested without owning the address.
///
/// # Errors
///
/// Returns [`ListenRefused`] on the same rules as [`parse_admin_listen`].
pub fn parse_admin_listen_with(
    raw: &str,
    assigned: &dyn Fn(IpAddr) -> bool,
) -> Result<SocketAddr, ListenRefused> {
    let addr: SocketAddr = match raw.parse() {
        Ok(addr) => addr,
        Err(_) => {
            let reason = if raw.parse::<IpAddr>().is_ok() {
                "the value is a bare address with no port, and a bare address is refused"
            } else {
                "this is not a literal ip:port; a hostname or a service name is refused"
            };
            return Err(ListenRefused::new(raw, reason));
        }
    };

    match classify(addr.ip()) {
        Class::Loopback => Ok(addr),
        Class::Tailnet => {
            if assigned(addr.ip()) {
                Ok(addr)
            } else {
                Err(ListenRefused::new(
                    raw,
                    format!(
                        "{} is in the tailnet range but is not assigned to any interface on this \
                         host",
                        addr.ip()
                    ),
                ))
            }
        }
        Class::Wildcard => Err(ListenRefused::new(
            raw,
            format!(
                "{} is the wildcard address, which is every interface at once",
                addr.ip()
            ),
        )),
        Class::Lan => Err(ListenRefused::new(
            raw,
            format!("{} is a private LAN address", addr.ip()),
        )),
        Class::Public => Err(ListenRefused::new(
            raw,
            format!(
                "{} is a public address, not this host's tailnet address",
                addr.ip()
            ),
        )),
        Class::NotTailnetV6 => Err(ListenRefused::new(
            raw,
            format!(
                "{} is not a tailnet address on this policy: only the IPv4 block 100.64.0.0/10 is \
                 accepted",
                addr.ip()
            ),
        )),
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Loopback => "loopback",
            Self::Tailnet => "tailnet",
            Self::Wildcard => "wildcard",
            Self::Lan => "lan",
            Self::Public => "public",
            Self::NotTailnetV6 => "ipv6-not-tailnet",
        };
        f.write_str(name)
    }
}

#[test]
fn the_default_is_still_loopback_only() {
    let default: SocketAddr = ADMIN_ADDR.parse().expect("a literal socket address");
    assert_eq!(default.to_string(), "127.0.0.1:7340");
    assert!(default.ip().is_loopback(), "{default} must be loopback");
    // Asking for the default by name is accepted and stays one listener: the
    // default never becomes a tailnet bind.
    assert_eq!(
        parse_admin_listen(ADMIN_ADDR).expect("the default is accepted"),
        default
    );
    assert!(
        !is_tailnet(default.ip()),
        "the default is not a tailnet bind"
    );
}

#[test]
fn a_tailnet_address_is_accepted() {
    let raw = "100.89.211.80:7340";
    let assigned = IpAddr::V4(Ipv4Addr::new(100, 89, 211, 80));
    let parsed = parse_admin_listen_with(raw, &|ip| ip == assigned)
        .expect("a tailnet address that is assigned locally is accepted");
    assert_eq!(parsed.to_string(), raw);
    assert!(is_tailnet(parsed.ip()));
    // The range alone is not enough: on a host where nothing owns the address,
    // the same literal is refused.
    let refused =
        parse_admin_listen_with(raw, &|_| false).expect_err("an address nothing owns is refused");
    assert!(
        refused.to_string().contains(
            "refusing --admin-listen 100.89.211.80:7340: the admin UI may bind only loopback \
             or this host's tailnet address"
        ),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("not assigned to any interface"),
        "{refused}"
    );
}

#[test]
fn a_lan_address_is_refused() {
    for raw in ["192.168.68.51:7340", "10.0.0.5:7340", "172.16.4.1:7340"] {
        let error = parse_admin_listen(raw).expect_err("a LAN address is refused");
        let message = error.to_string();
        assert!(
            message.starts_with(&format!(
                "refusing --admin-listen {raw}: the admin UI may bind only loopback or this \
                 host's tailnet address"
            )),
            "{message}"
        );
        assert!(message.contains("private LAN address"), "{message}");
    }
}

#[test]
fn a_wildcard_address_is_refused() {
    for raw in ["0.0.0.0:7340", "[::]:7340"] {
        let error = parse_admin_listen(raw).expect_err("a wildcard is refused");
        let message = error.to_string();
        assert!(
            message.starts_with(&format!(
                "refusing --admin-listen {raw}: the admin UI may bind only loopback or this \
                 host's tailnet address"
            )),
            "{message}"
        );
        assert!(message.contains("wildcard address"), "{message}");
    }
}

#[test]
fn a_public_address_is_refused() {
    for raw in ["8.8.8.8:7340", "1.1.1.1:7340"] {
        let error = parse_admin_listen(raw).expect_err("a public address is refused");
        let message = error.to_string();
        assert!(
            message.starts_with(&format!(
                "refusing --admin-listen {raw}: the admin UI may bind only loopback or this \
                 host's tailnet address"
            )),
            "{message}"
        );
        assert!(message.contains("public address"), "{message}");
    }
}

#[test]
fn a_hostname_is_refused() {
    for raw in [
        "localhost:7340",
        "fih-ai-host.tail4d497d.ts.net:7340",
        "example.com:7340",
    ] {
        let error = parse_admin_listen(raw).expect_err("a hostname is refused");
        let message = error.to_string();
        assert!(
            message.starts_with(&format!(
                "refusing --admin-listen {raw}: the admin UI may bind only loopback or this \
                 host's tailnet address"
            )),
            "{message}"
        );
        assert!(
            message.contains("not a literal ip:port") || message.contains("bare address"),
            "{message}"
        );
    }
}

#[tokio::test]
async fn both_listeners_serve_the_same_healthz() {
    let dir = tempfile::tempdir().expect("a temporary state root");
    let db = crate::db::Db::open(dir.path().join("eris.db")).expect("a fresh database");
    let paths = crate::config::Paths::for_root(dir.path());
    let state = AppState::new(crate::onboarding::SetupEngine::new(db, paths), false);

    // Two listeners, one `AppState`, one router: the shape `serve_with` builds
    // when `--admin-listen` is given. Ephemeral ports keep the test independent
    // of whatever else is on this host.
    let first = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback listener");
    let first_addr = first.local_addr().expect("a local address");
    let second = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a second ephemeral loopback listener");
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
