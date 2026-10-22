#!/bin/sh
# Kallisti-Eris installer for Linux.
#
# The short form:
#
#   curl -fsSL https://raw.githubusercontent.com/fireishott/Kallisti/main/install.sh | sh
#
# What it does, in order:
#
#   1. Works out the architecture (x86_64 or aarch64) and the newest release tag.
#   2. Installs a prebuilt binary from that release when one exists. No Rust, no
#      compiler, no ten-minute build.
#   3. Falls back to building from source with the pinned toolchain when there is
#      no prebuilt asset for this machine.
#   4. Puts the binary at ~/.local/opt/kallisti/bin/eris, linked into ~/.local/bin.
#   5. Creates the config, state and workspace directories, plus a commented
#      eris.env and a keys.env the daemon needs in order to boot.
#   6. Writes and starts a systemd *user* unit, unless --no-service.
#
# Nothing here needs root. Nothing here touches a file outside $HOME.
#
# Flags:
#   --from-source        build from source even if a prebuilt binary exists
#   --version <tag>      install a specific release, e.g. v1.4.1
#   --prefix <dir>       where the binary lives   (default ~/.local/opt/kallisti)
#   --bin-dir <dir>      where the symlink lives  (default ~/.local/bin)
#   --repo <owner/name>  source repository        (default fireishott/Kallisti)
#   --no-service         do not write or start the systemd user unit
#   --dry-run            print the plan and change nothing
#   -h, --help           this text
#
# Environment overrides, for a mirrored install:
#   ERIS_TARBALL_URL     a tarball to install instead of resolving a release
#   ERIS_SOURCE_DIR      an existing checkout to build instead of cloning

set -eu

REPO="fireishott/Kallisti"
PREFIX="$HOME/.local/opt/kallisti"
BIN_DIR="$HOME/.local/bin"
VERSION=""
FROM_SOURCE=0
NO_SERVICE=0
DRY_RUN=0

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
run() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '  would run: %s\n' "$*"
    else
        "$@"
    fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --from-source) FROM_SOURCE=1 ;;
        --no-service) NO_SERVICE=1 ;;
        --dry-run) DRY_RUN=1 ;;
        --version) shift; [ $# -gt 0 ] || die "--version needs a tag"; VERSION="$1" ;;
        --prefix) shift; [ $# -gt 0 ] || die "--prefix needs a path"; PREFIX="$1" ;;
        --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a path"; BIN_DIR="$1" ;;
        --repo) shift; [ $# -gt 0 ] || die "--repo needs owner/name"; REPO="$1" ;;
        -h|--help) sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done

# --- 1. Platform -------------------------------------------------------------

[ "$(uname -s)" = "Linux" ] || die "this installer is Linux only; there is no macOS or Windows build in this repo yet"

case "$(uname -m)" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
    *) die "unsupported architecture: $(uname -m). Assets exist for x86_64 and aarch64" ;;
esac

if [ -z "$VERSION" ]; then
    VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=1" 2>/dev/null \
        | grep -o '"tag_name" *: *"[^"]*"' | head -1 | cut -d'"' -f4 || true)
    [ -n "$VERSION" ] || VERSION="main"
fi

say "Kallisti-Eris installer"
say "  repo      $REPO"
say "  version   $VERSION"
say "  platform  linux/$ARCH"
say "  prefix    $PREFIX"
say ""

# --- 2. Prebuilt binary ------------------------------------------------------

TARBALL="${ERIS_TARBALL_URL:-https://github.com/$REPO/releases/download/$VERSION/eris-$VERSION-linux-$ARCH.tar.gz}"
TMP=$(mktemp -d 2>/dev/null || mktemp -d -t eris-install)
trap 'rm -rf "$TMP"' EXIT INT TERM

installed_from=""

install_binary() {
    [ -x "$1" ] || die "not an executable: $1"
    run mkdir -p "$PREFIX/bin"
    run install -m 755 "$1" "$PREFIX/bin/eris"
    installed_from="$2"
}

if [ "$FROM_SOURCE" = 0 ]; then
    if [ "$DRY_RUN" = 1 ]; then
        say "Would try a prebuilt binary:"
        say "  $TARBALL"
    elif curl -fsSL -o "$TMP/eris.tar.gz" "$TARBALL" 2>/dev/null; then
        if curl -fsSL -o "$TMP/eris.sha256" "$TARBALL.sha256" 2>/dev/null && command -v sha256sum >/dev/null 2>&1; then
            expected=$(cut -d' ' -f1 "$TMP/eris.sha256")
            actual=$(sha256sum "$TMP/eris.tar.gz" | cut -d' ' -f1)
            [ "$expected" = "$actual" ] || die "checksum mismatch on the downloaded tarball"
            say "Checksum verified."
        else
            warn "no .sha256 beside the tarball, so nothing was verified"
        fi
        run tar xzf "$TMP/eris.tar.gz" -C "$TMP"
        found=$(find "$TMP" -type f -name eris -perm -u+x | head -1)
        [ -n "$found" ] || die "the tarball carried no eris binary"
        install_binary "$found" "prebuilt $VERSION"
        say "Installed the prebuilt binary."
    else
        warn "no prebuilt asset for linux/$ARCH at $VERSION - building from source"
        FROM_SOURCE=1
    fi
fi

# --- 3. Source build ---------------------------------------------------------

if [ -z "$installed_from" ]; then
    missing=""
    for tool in curl tar git cc; do
        command -v "$tool" >/dev/null 2>&1 || missing="$tool${missing:+ $missing}"
    done
    if [ -n "$missing" ]; then
        printf 'error: missing build tools: %s\n' "$missing" >&2
        say ""
        say "On Debian or Ubuntu:"
        say "  sudo apt update && sudo apt install -y curl git build-essential"
        die "install those and run this again"
    fi

    # A build needs the toolchain the repo pins. rustup is the only reliable way
    # to get exactly that version; a distro cargo is usually years off.
    CARGO=""
    for candidate in "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" "$(command -v cargo 2>/dev/null || true)"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then CARGO="$candidate"; break; fi
    done
    if [ -z "$CARGO" ]; then
        say "No Rust toolchain found. Installing rustup into ~/.cargo ..."
        run sh -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none'
        CARGO="${CARGO_HOME:-$HOME/.cargo}/bin/cargo"
        if [ "$DRY_RUN" = 0 ] && [ ! -x "$CARGO" ]; then
            die "rustup finished but $CARGO is not there"
        fi
    fi
    PATH="$(dirname "$CARGO"):$PATH"
    export PATH

    if [ -n "${ERIS_SOURCE_DIR:-}" ]; then
        SRC="$ERIS_SOURCE_DIR"
        say "Building from the checkout at $SRC"
    else
        SRC="$TMP/src"
        if [ "$VERSION" = "main" ]; then
            run git clone --depth 1 "https://github.com/$REPO.git" "$SRC"
        else
            run git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$SRC" \
                || run git clone --depth 1 "https://github.com/$REPO.git" "$SRC"
        fi
    fi

    say "Building - the slow path, about ten minutes on a modest box ..."
    if [ "$DRY_RUN" = 1 ]; then
        printf '  would run: cargo build --release --locked (in %s)\n' "$SRC"
    else
        ( cd "$SRC" && "$CARGO" build --release --locked ) || die "the build failed; the compiler error is above"
        install_binary "$SRC/target/release/eris" "source $VERSION"
    fi
fi

# --- 4. Directories, config template, key file -------------------------------

CONFIG_DIR="$HOME/.config/kallisti"
STATE_DIR="$HOME/.local/share/kallisti"
WORKSPACE="$HOME/kallisti-workspace"
ENV_FILE="$CONFIG_DIR/eris.env"
KEYS_FILE="$CONFIG_DIR/keys.env"

run mkdir -p "$CONFIG_DIR" "$STATE_DIR" "$WORKSPACE" "$BIN_DIR"

if [ "$DRY_RUN" = 0 ] && [ ! -e "$BIN_DIR/eris" ]; then
    ln -s "$PREFIX/bin/eris" "$BIN_DIR/eris"
fi

if [ "$DRY_RUN" = 0 ] && [ ! -f "$ENV_FILE" ]; then
    umask 077
    cat > "$ENV_FILE" <<'ENVEOF'
# Kallisti-Eris provider settings. Mode 0600 is required: the daemon refuses to
# start if this file is readable by anyone else.
#
# Point these three at any OpenAI-compatible endpoint. The key itself never
# appears in this file, only the *name* of an environment variable that holds
# it. That variable is set in keys.env, beside this file.
KALLISTI_BASE_URL=http://127.0.0.1:11434/v1
KALLISTI_MODEL=llama3.1
KALLISTI_API_KEY_REF=env:ERIS_API_KEY
ENVEOF
    chmod 600 "$ENV_FILE"
    say "Wrote a starter $ENV_FILE - edit the three lines for your endpoint."
fi

if [ "$DRY_RUN" = 0 ] && [ ! -f "$KEYS_FILE" ]; then
    umask 077
    cat > "$KEYS_FILE" <<'KEYEOF'
# The provider key lives here, and only here. ERIS_API_KEY is the name that
# KALLISTI_API_KEY_REF in eris.env points at, so the value never lands in a file
# that the daemon reads as configuration.
#
# ERIS_API_KEY=sk-...
KEYEOF
    chmod 600 "$KEYS_FILE"
    say "Wrote $KEYS_FILE - put your provider key in it."
fi

# --- 5. systemd user unit ----------------------------------------------------

UNIT_DIR="$HOME/.config/systemd/user"
UNIT="$UNIT_DIR/kallisti-eris.service"

write_unit() {
    mkdir -p "$UNIT_DIR"
    cat > "$UNIT" <<UNITEOF
[Unit]
Description=Kallisti Eris daemon (web UI on 127.0.0.1:7340, device API on :7341)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$PREFIX/bin/eris serve
# eris.env holds the provider settings; keys.env holds the key that the
# reference inside eris.env names. The second file is optional, hence the dash.
EnvironmentFile=$ENV_FILE
EnvironmentFile=-$KEYS_FILE
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=5
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=read-only
# The daemon writes its database, its workspace and its config, nothing else.
ReadWritePaths=$STATE_DIR $WORKSPACE $CONFIG_DIR
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
MemoryHigh=2G
MemoryMax=4G

[Install]
WantedBy=default.target
UNITEOF
}

if [ "$NO_SERVICE" = 1 ]; then
    say ""
    say "Skipping the service. Start it by hand with:"
    say "  $PREFIX/bin/eris serve"
elif ! command -v systemctl >/dev/null 2>&1; then
    warn "no systemctl on this host, so no unit was written"
    say "Start it by hand with: $PREFIX/bin/eris serve"
elif [ "$DRY_RUN" = 1 ]; then
    printf '  would write: %s\n' "$UNIT"
    printf '  would run: systemctl --user daemon-reload; systemctl --user enable --now kallisti-eris\n'
else
    write_unit
    systemctl --user daemon-reload
    if systemctl --user enable --now kallisti-eris.service; then
        say "Started kallisti-eris.service."
    else
        warn "the unit was written but did not start: journalctl --user -u kallisti-eris -n 40"
    fi
    if command -v loginctl >/dev/null 2>&1; then
        linger=$(loginctl show-user "$USER" -p Linger 2>/dev/null | cut -d= -f2 || true)
        if [ "$linger" != "yes" ]; then
            say ""
            say "Linger is off, so the daemon stops when you log out. To keep it up:"
            say "  sudo loginctl enable-linger $USER"
        fi
    fi
fi

# --- 6. What now -------------------------------------------------------------

say ""
if [ -n "$installed_from" ]; then
    say "Installed $("$PREFIX/bin/eris" --version 2>/dev/null || echo eris) ($installed_from)."
fi
say "Next:"
say "  1. Put your provider key in $KEYS_FILE"
say "  2. Check the endpoint and model in $ENV_FILE"
say "  3. Open http://127.0.0.1:7340"
say "     The six-word setup passphrase is printed on first start and saved at"
say "     $CONFIG_DIR/first-run.txt"
say ""
say "If $BIN_DIR is not on your PATH, call the binary at $PREFIX/bin/eris."
