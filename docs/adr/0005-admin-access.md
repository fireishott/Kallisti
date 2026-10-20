# ADR 0005 - Admin access: simultaneous listeners and trusted proxies

- Status: **implemented** (P15)
- Date: 2026-10-19
- Wave: P15
- Related: `crates/eris-host/src/admin/listen.rs`,
  `crates/eris-host/src/web/proxy.rs`, `crates/eris-host/src/config.rs`,
  `~/.config/kallisti/config.toml`, `kallisti-eris.service`
- Supersedes: the loopback-literal bind and the forwarded-header refusal in
  ADR-era plan section 10.4

## Context

1.0 shipped with one admin listener, `127.0.0.1:7340`, written as a `const`. The
config loader refused a `listen` key on purpose. `--admin-listen` could add one
more bind, but only a loopback address or this host's own Tailscale address, and
only after a "is this address assigned here" probe. A LAN address was refused
outright.

The request that forced this ADR: the admin UI must be reachable from the host,
from a phone or laptop on the LAN, from the tailnet, and through a reverse proxy
or a Cloudflare tunnel, and those paths have to work *together* rather than as an
either/or. The old design could not do any of them well:

- a LAN bind was refused, so no phone on the couch could open the UI;
- the bind list was not configurable, so every exposure was a command-line edit
  to a systemd unit, and a service restart per experiment;
- `reject_forwarded` answered 403 `not_loopback` to any request carrying
  `X-Forwarded-For`, `X-Forwarded-Host`, `X-Forwarded-Proto`, `Forwarded` or any
  `Tailscale-*` header. A proxy or a tunnel in front of the daemon could never
  work at all, and the refusal did not distinguish a proxy from a spoof.

The property that actually matters is not "the socket is loopback". It is "a
remote client cannot convince this daemon that it is someone else". A literal
bind address was a proxy for that property, and a bad one.

## Decision

### 1. The listener list is config, and every entry is served

`~/.config/kallisti/config.toml`:

```toml
[admin]
listen = ["127.0.0.1:7340"]
public_url = "https://kallisti.example.com"
trusted_proxies = ["127.0.0.1/32", "::1/128"]
```

- Every key is optional and none of them is a secret.
- An absent or empty `listen` means exactly `127.0.0.1:7340`.
- Every listed address binds, and all of them serve the same router. There is no
  "primary" listener in the daemon; the first entry is only what the first-run
  block and the footer call the primary.
- `serve --admin-listen <ip:port>` is repeatable and additive: it appends to the
  configured list, so a one-off exposure needs no file edit.
- An entry that is not a literal `ip:port` (a hostname, a URL) is a startup
  error naming the value. The same address listed twice is a startup error too,
  because the second bind would fail with the address already in use.
- `0.0.0.0` and `::` are accepted, and every startup that binds one logs a
  warning at `warn` naming the exposure.
- A bind failure is loud: every address must bind before any of them serves.

The addresses that actually bound are stored in `AppState` and rendered: the
dashboard System card lists all of them plus the public URL, the footer states
the primary address and "(N listeners)" when there is more than one, and the
Settings Network row says the UI is reachable on the host, on a LAN address, on
a tailnet address, or through a configured reverse proxy.

Nothing in the code or the unit names a tailnet address. The old Tailscale
`100.64.0.0/10` special case is gone, including the local-assignment probe.

### 2. Forwarding headers are believed by peer, not banned by header

The middleware now derives a `ClientContext` per request:

- the immediate peer comes from `ConnectInfo<SocketAddr>` (the daemon serves with
  `into_make_service_with_connect_info`);
- if that peer is inside `admin.trusted_proxies`, the effective client IP is the
  first usable `X-Forwarded-For` hop (then `X-Real-IP`, then the peer), the
  effective scheme is `X-Forwarded-Proto` when it says `http` or `https`, and the
  effective host is `X-Forwarded-Host` or `X-Forwarded-Server`;
- the context lands in the request extensions, so handlers, the audit actor and
  the rate limiter use the real client. A sign-in audited through a proxy records
  the client address as the actor, not `127.0.0.1`; the claim and sign-in rate
  limits are per effective client, so one proxy cannot pool every client into a
  single budget and a client cannot escape its own budget by arriving through one.

If the peer is **not** trusted and a forwarding header is present, the answer is
still `403 not_loopback`. That is not a compatibility decision: a header that
claims a different client, arriving from an address that is not allowed to make
that claim, is a spoof attempt. The 403 is unchanged, code and all.

`trusted_proxies` accepts IPv4 and IPv6 CIDRs, and a bare address as `/32` or
`/128`. An unparsable entry is a config error naming the value. The default is
loopback only (`127.0.0.1/32`, `::1/128`), which is what Tailscale Serve, a local
nginx and a local Caddy all are.

### 3. The session cookie's `Secure` flag follows the scheme

`public_url` is used for absolute links and for the cookie decision:
`https://...` sets `Secure`; so does a request that arrived over a trusted
`X-Forwarded-Proto: https`. A plain loopback HTTP page never sets it, because a
browser silently drops a `Secure` cookie it received over HTTP and the operator
would be locked out of their own wizard. `HttpOnly`, `SameSite=Strict` and
`Path=/` are unconditional.

## Why the default stays loopback

A default that binds every interface turns any host with a working network into
an exposed admin surface, and the failure mode is silent: nobody notices a
listener they did not ask for. The default must be the safe one, and it must be
the one that needs no explanation: `127.0.0.1:7340`, exactly as before, with
loopback-trusted proxies so a local nginx, Caddy or Tailscale Serve works with no
config edit.

The safe default costs one line of config to change. The unsafe default costs an
incident.

## What binding `0.0.0.0` actually means

`listen = ["127.0.0.1:7340", "0.0.0.0:7340"]` does not mean "LAN and tailnet". It
means every IPv4 interface this host has, present and future, including a
temporary docker bridge, a VPN adapter that appears at 3am, and a tethering
interface on a laptop that travels. It also makes the loopback entry redundant
for IPv4 while keeping it for the sake of an obvious local check.

Anyone who can reach any of those addresses can reach the admin surface. What
they meet there is the login form, the claim form with its per-client rate limit,
and a session cookie that is `HttpOnly` and `SameSite=Strict`. What they do not
meet is TLS: this daemon does not terminate TLS and will not. Over plain HTTP on
a shared network, the admin passphrase crosses in the clear. That is why the
wildcard bind is accepted with a warning rather than by default, and why every
example below that crosses a network puts TLS in front of it.

## Worked examples

Each example states the exact config, then the security consequence. In all of
them the state database stays in `~/.local/share/kallisti/` and the provider key
stays in the environment behind `env:NINEROUTER_API_KEY`.

### (a) Host only: the default

```toml
[admin]
listen = ["127.0.0.1:7340"]
```

Reach it at `http://127.0.0.1:7340`, or tunnel in with
`ssh -L 7340:127.0.0.1:7340 operator@the daemon host`.

Consequence: no other machine can open a socket to the listener. An SSH tunnel
gives a remote operator access without exposing a port, at the cost of a login.
Nothing on the network can reach the UI, so there is nothing to spoof and nothing
to sniff.

### (b) LAN

```toml
[admin]
listen = ["127.0.0.1:7340", "192.168.1.10:7340"]
trusted_proxies = ["127.0.0.1/32", "::1/128"]
```

Reach it at `http://192.168.1.10:7340` from any device on `192.168.1.0/24`.

Consequence: every device on the LAN is one TCP connection away from the login
form, over plain HTTP. The admin passphrase is typed across a shared network.
Anyone who can ARP-spoof or read that LAN can capture it. Bind the host's real LAN
address, not `0.0.0.0`, so a new interface (a VPN, a docker bridge) does not
silently join the surface. If the LAN is not entirely trusted, put nginx or Caddy
in front with TLS (example e) and leave the daemon on loopback.

### (c) Tailscale

```toml
[admin]
listen = ["127.0.0.1:7340", "100.64.0.10:7340"]
```

Reach it at `http://100.64.0.10:7340` from any device on the tailnet, with no
`tailscale serve` and no tunnel. (`tailscale serve` in front of loopback is the
other, equally valid shape, and it needs no `trusted_proxies` change because Serve
connects from `127.0.0.1`.)

Consequence: the tailnet ACL, not a LAN, decides who can reach the admin port.
That is better than the LAN example, and it is still plain HTTP inside the tailnet:
Tailscale encrypts the hop to the host, so the wire is protected, but there is no
TLS at the application layer and no certificate, so a browser shows a warned page
or an untrusted-origin prompt, and `Secure` cannot be set on the cookie unless a
`public_url` says the front end is https.

### (d) Cloudflare Tunnel

```toml
[admin]
listen = ["127.0.0.1:7340"]
public_url = "https://kallisti.example.com"
trusted_proxies = ["127.0.0.1/32", "::1/128"]
```

`cloudflared` runs on the same host and connects to `http://127.0.0.1:7340`, so
its peer is loopback and it is trusted by default. The app never terminates TLS;
Cloudflare does. Absolute links use `https://kallisti.example.com`, and the
session cookie carries `Secure`.

Consequence: reachable from the internet through Cloudflare's edge, which means
Cloudflare sees the request and the response including the admin page and, on the
sign-in POST, the passphrase body. The daemon's only view of the client is the
`X-Forwarded-For` hop Cloudflare sends, which is why the audit trail records it.
Do not add `0.0.0.0` to the listener list for this shape: the tunnel is the only
path that should reach it. If cloudflared runs in a container or on another host,
its peer is not loopback, so add that peer's address to `trusted_proxies`
deliberately and nothing wider.

### (e) nginx or Caddy with TLS

```toml
[admin]
listen = ["127.0.0.1:7340"]
public_url = "https://kallisti.example.com"
trusted_proxies = ["127.0.0.1/32", "::1/128"]
```

```nginx
server {
    listen 443 ssl;
    server_name kallisti.example.com;
    location / {
        proxy_pass http://127.0.0.1:7340;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
    }
}
```

Caddy is the same shape: `reverse_proxy 127.0.0.1:7340` sets the forwarded
headers itself and terminates TLS with an automatic certificate.

Consequence: TLS ends at the proxy, so the admin passphrase crosses the loopback
hop in clear text inside the host and nothing else. The proxy is trusted because
it is local; if it is on another machine, the hop between it and the daemon is
plain HTTP across that network, which is worse than example (b) and needs a
private network or TLS on that hop too. Never set `trusted_proxies = ["0.0.0.0/0"]`:
that trusts any peer to claim any client address, which is the spoofing hole the
403 exists to close.

## Alternatives rejected

- **"Loopback or nothing, SSH tunnel only."** It works and it was the old
  behaviour, but it cannot serve a phone on the LAN and it cannot sit behind a
  proxy, which is the requirement.
- **"Bind `0.0.0.0` by default and rely on authentication."** A safe-by-accident
  default is not a default. The admin port would be open on every host that runs
  the daemon.
- **"Keep rejecting every forwarded header and require a tunnel for every remote
  path."** It made the proxy case impossible while pretending to defend against
  spoofing that the peer check defends against properly.
- **"Trust `X-Forwarded-For` from anyone."** That is the spoofing hole: any client
  could write its own identity into the audit trail and its own rate-limit bucket.
- **"Add a new crate for CIDR parsing."** One small type with two mask functions
  is enough, and the no-new-dependency rule is worth more than the saved lines.
- **"Terminate TLS in the daemon."** Out of scope, and it would put certificate
  lifecycle inside the admin process. A reverse proxy does it better.

## Tests that pin this

- `admin::listen::the_default_list_is_loopback_only`
- `admin::listen::multiple_addresses_are_all_bound`
- `admin::listen::a_wildcard_address_is_accepted_when_asked_for`
- `admin::listen::a_duplicate_address_is_a_config_error`
- `admin::listen::a_malformed_address_is_a_config_error`
- `admin::listen::the_flag_adds_to_the_config_list`
- `web::proxy::a_trusted_peer_may_send_forwarded_headers`
- `web::proxy::an_untrusted_peer_sending_forwarded_headers_is_403`
- `web::proxy::the_client_context_reports_the_forwarded_address_and_scheme`
- `web::proxy::a_bad_cidr_is_a_config_error`
- `web::proxy::an_https_public_url_makes_the_cookie_secure`
- `web::shell::the_dashboard_lists_every_bound_listener`
