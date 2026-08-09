#!/usr/bin/env python3
"""One-time login that gives the connector's native watcher its own credential.

Run this ONCE, on a machine with a browser, on the same network as the
gateway. It performs the same RFC 8252 broker flow the iOS app uses and
writes the resulting refresh token to a config file for the connector.

Why the watcher needs its own login rather than reusing the app's: Portal
rotates refresh tokens on every use and runs reuse-detection. Two clients
sharing one refresh token would rotate it out from under each other and
trip that detection, revoking the whole session. Separate sessions are
required, not merely tidier.

    python3 setup_native_watch_login.py --gateway https://your-gateway.example

Then copy the printed file to the connector host at the path it names.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.server
import json
import secrets
import threading
import urllib.parse
import urllib.request
import webbrowser

DEFAULT_CONFIG_NAME = "kallisti-native-watch.json"

_callback: dict[str, str] = {}
_callback_event = threading.Event()


class _CallbackHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - stdlib naming
        query = urllib.parse.urlparse(self.path).query
        params = urllib.parse.parse_qs(query)
        _callback["code"] = params.get("code", [""])[0]
        _callback["state"] = params.get("state", [""])[0]
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        self.wfile.write(
            b"<html><body>Connector authorized. You can close this tab.</body></html>"
        )
        _callback_event.set()

    def log_message(self, *args) -> None:  # noqa: A002 - silence stdlib logging
        pass


def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gateway",
        required=True,
        help="Gateway base URL, e.g. https://hermes-relay.example.net",
    )
    parser.add_argument(
        "--provider",
        default="nous",
        help="Auth provider name (default: nous). Required explicitly when "
        "more than one session provider is registered.",
    )
    parser.add_argument("--port", type=int, default=8934, help="Local callback port")
    parser.add_argument("--out", default=DEFAULT_CONFIG_NAME, help="Output file")
    args = parser.parse_args()

    gateway = args.gateway.rstrip("/")
    verifier = _b64url(secrets.token_bytes(32))
    challenge = _b64url(hashlib.sha256(verifier.encode("ascii")).digest())
    state = secrets.token_urlsafe(16)
    redirect_uri = f"http://127.0.0.1:{args.port}/callback"

    server = http.server.HTTPServer(("127.0.0.1", args.port), _CallbackHandler)
    threading.Thread(target=server.handle_request, daemon=True).start()

    authorize_url = f"{gateway}/auth/native/authorize?" + urllib.parse.urlencode(
        {
            "provider": args.provider,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "redirect_uri": redirect_uri,
            "state": state,
        }
    )
    print(f"Opening browser to authorize the connector:\n  {authorize_url}\n")
    webbrowser.open(authorize_url)

    if not _callback_event.wait(timeout=300):
        raise SystemExit("Timed out waiting for the browser login.")
    if _callback.get("state") != state:
        raise SystemExit("State mismatch — aborting rather than trusting the callback.")
    if not _callback.get("code"):
        raise SystemExit("No authorization code returned.")

    request = urllib.request.Request(
        f"{gateway}/auth/native/token",
        method="POST",
        headers={"Content-Type": "application/json"},
        data=json.dumps(
            {"code": _callback["code"], "code_verifier": verifier}
        ).encode(),
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        tokens = json.load(response)

    refresh_token = tokens.get("refresh_token")
    if not refresh_token:
        raise SystemExit(
            "Gateway returned no refresh_token — the watcher needs one to "
            "keep running unattended."
        )

    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump({"refresh_token": refresh_token}, handle)

    print(f"Wrote {args.out}\n")
    print("Install it on the connector host, then restart the connector:")
    print(f"  scp {args.out} <user>@<host>:~/.config/{DEFAULT_CONFIG_NAME}")
    print(f"  ssh <user>@<host> 'chmod 600 ~/.config/{DEFAULT_CONFIG_NAME} && "
          "systemctl --user restart hermes-mobile-connector.service'")
    print(f"\nThen delete your local copy: rm {args.out}")


if __name__ == "__main__":
    main()
