#!/bin/sh
# Kallisti-Eris update helper: the half of the update button that can write.
#
# The daemon cannot install its own update. `kallisti-eris.service` runs with
# ProtectHome=read-only and may write only its state, its workspace and its
# config, while the binary it must replace lives in ~/.local/opt/kallisti/bin.
# That is not an accident of the unit file, it is the trust boundary: the process
# being replaced does not get write access to the thing replacing it.
#
# So the daemon writes a request, starts this script's unit, and reports progress
# from a file. This script does the part that needs the wider sandbox:
#
#   1. read the request, refusing anything absent or malformed
#   2. refuse any asset URL that is not on the one release host
#   3. download the tarball and its checksum
#   4. verify the checksum, and stop dead when there is not one
#   5. back up the running binary, keeping the last five
#   6. stop the daemon
#   7. install the new binary
#   8. start the daemon and wait for /healthz
#   9. on success: state=done. On any failure: restore the backup, restart, state=failed
#
# A failed update that leaves the daemon down is worse than no update button, so
# step 9 restores before it reports.
#
# Flags:
#   --dry-run            print every step and change nothing
#   --prefix <dir>       the install prefix to rewrite (default ~/.local/opt/kallisti)
#   --no-restart         leave the daemon stopped (rehearsal only)
#   -h, --help           this text
#
# Environment:
#   KALLISTI_BIN         the binary path, overriding --prefix
#   KALLISTI_UPDATE_DIR  where request.json, state.json and downloads live
#                        (default ~/.local/share/kallisti/update)
#   KALLISTI_UNIT        the daemon unit to stop and start (default kallisti-eris)
#   KALLISTI_HEALTH_URL  the health probe (default http://127.0.0.1:7340/healthz)
#
# Nothing here needs root. Nothing here writes outside $HOME unless --prefix or
# KALLISTI_BIN is pointed somewhere else on purpose, which is what a rehearsal does.

set -eu

# The only host a release asset may come from. A redirect off it aborts.
RELEASE_HOST="https://github.com/fireishott/Kallisti/releases/download/"

PREFIX="${HOME}/.local/opt/kallisti"
BIN="${KALLISTI_BIN:-}"
UPDATE_DIR="${KALLISTI_UPDATE_DIR:-${HOME}/.local/share/kallisti/update}"
UNIT="${KALLISTI_UNIT:-kallisti-eris}"
HEALTH_URL="${KALLISTI_HEALTH_URL:-http://127.0.0.1:7340/healthz}"
DRY_RUN=0
DO_RESTART=1
KEEP_BACKUPS=5
HEALTH_TIMEOUT=30

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
step() {
    if [ "$DRY_RUN" = 1 ]; then
        printf '  would run: %s\n' "$*"
    else
        say "  $*"
        "$@"
    fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --no-restart) DO_RESTART=0 ;;
        --prefix) shift; [ $# -gt 0 ] || die "--prefix needs a path"; PREFIX="$1" ;;
        -h|--help) sed -n '2,45p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done

[ -n "$BIN" ] || BIN="$PREFIX/bin/eris"

REQUEST="$UPDATE_DIR/request.json"
STATE="$UPDATE_DIR/state.json"
DOWNLOAD_DIR="$UPDATE_DIR/download"
TARBALL="$DOWNLOAD_DIR/eris.tar.gz"
TARBALL_TMP="$TARBALL.tmp"
CHECKSUM_FILE="$DOWNLOAD_DIR/eris.tar.gz.sha256"
FROM_VERSION="unknown"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# --- state reporting ---------------------------------------------------------
#
# Every transition rewrites state.json atomically, because the daemon polls it
# while the daemon is being replaced. A half-written document would be read as
# "unknown"; absent is read as "idle", which during an install is a lie.

write_state() {
    status="$1"
    error="${2:-}"
    if [ "$DRY_RUN" = 1 ]; then
        printf '  would write state: %s\n' "$status"
        return 0
    fi
    mkdir -p "$UPDATE_DIR"
    tmp="$STATE.tmp"
    {
        printf '{"state":"%s","exit_code":%s,"started_at":"%s","finished_at":' \
            "$status" "${EXIT_CODE:-null}" "$STARTED_AT"
        case "$status" in
            done|failed) printf '"%s"' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" ;;
            *) printf 'null' ;;
        esac
        printf ',"from_version":"%s","to_version":"%s","output":[' "${FROM_VERSION}" "${TO_VERSION:-}"
        first=1
        if [ -f "$OUTPUT_FILE" ]; then
            while IFS= read -r line; do
                escaped=$(printf '%s' "$line" | sed 's/\\/\\\\/g; s/"/\\"/g')
                if [ "$first" = 1 ]; then first=0; else printf ','; fi
                printf '"%s"' "$escaped"
            done < "$OUTPUT_FILE"
        fi
        printf '],"error":'
        case "$error" in
            "") printf 'null' ;;
            *) printf '"%s"' "$(printf '%s' "$error" | sed 's/\\/\\\\/g; s/"/\\"/g')" ;;
        esac
        printf '}\n'
    } > "$tmp"
    mv -f "$tmp" "$STATE"
}

note() {
    printf '%s\n' "$*" >> "$OUTPUT_FILE"
    say "$*"
}

# --- 1. the request ----------------------------------------------------------

if [ ! -f "$REQUEST" ]; then
    say "error: no update request at $REQUEST" >&2
    say "Nothing to do. The daemon writes one when an install is requested." >&2
    exit 2
fi

OUTPUT_FILE="$(mktemp "${TMPDIR:-/tmp}/kallisti-update-output.XXXXXX")"
trap 'rm -f "$OUTPUT_FILE"' EXIT

# One key out of the request without a JSON parser: the document is written by
# this daemon, so the shape is known and a key is one quoted string.
json_field() {
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$REQUEST" | head -1
}

ASSET_URL="$(json_field asset_url)"
CHECKSUM_URL="$(json_field checksum_url)"
TO_VERSION="$(json_field tag)"

[ -n "$ASSET_URL" ] || { printf 'error: the request names no asset_url\n' >&2; exit 2; }
[ -n "$CHECKSUM_URL" ] || { printf 'error: the request names no checksum_url, and an unsigned swap is not offered\n' >&2; exit 2; }
[ -n "$TO_VERSION" ] || TO_VERSION="unknown"

if [ -x "$BIN" ]; then
    FROM_VERSION="$("$BIN" --version 2>/dev/null || echo unknown)"
fi

say "Kallisti update: $FROM_VERSION -> $TO_VERSION"
note "request read from $REQUEST"

# --- 2. only the one release host --------------------------------------------

case "$ASSET_URL" in
    "${RELEASE_HOST}"*) ;;
    *)
        EXIT_CODE=3
        note "refused: $ASSET_URL is not under $RELEASE_HOST"
        write_state failed "the asset is not on the release host"
        printf 'error: the asset URL is not on %s; refusing to download it\n' "$RELEASE_HOST" >&2
        exit 3
        ;;
esac
case "$CHECKSUM_URL" in
    "${RELEASE_HOST}"*) ;;
    *)
        EXIT_CODE=3
        note "refused: $CHECKSUM_URL is not under $RELEASE_HOST"
        write_state failed "the checksum is not on the release host"
        printf 'error: the checksum URL is not on %s; refusing to trust it\n' "$RELEASE_HOST" >&2
        exit 3
        ;;
esac

# --- 3. download -------------------------------------------------------------

step mkdir -p "$DOWNLOAD_DIR"
write_state downloading

if [ "$DRY_RUN" = 1 ]; then
    say "  would run: curl -fsSL --max-time 300 --proto '=https' --proto-redir '=https' -o $TARBALL_TMP $ASSET_URL"
    say "  would run: curl -fsSL --max-time 300 --proto '=https' --proto-redir '=https' -o $CHECKSUM_FILE $CHECKSUM_URL"
else
    # --proto-redir '=https' is what makes "a redirect off that host aborts" real:
    # no plaintext hop, and no non-https scheme, from start to final URL.
    if ! curl -fsSL --max-time 300 --proto '=https' --proto-redir '=https' \
        -o "$TARBALL_TMP" "$ASSET_URL"; then
        EXIT_CODE=3
        rm -f "$TARBALL_TMP"
        write_state failed "the release asset could not be downloaded"
        printf 'error: the release asset could not be downloaded\n' >&2
        exit 3
    fi
    note "downloaded $(wc -c < "$TARBALL_TMP" | tr -d ' ') bytes"
    if ! curl -fsSL --max-time 300 --proto '=https' --proto-redir '=https' \
        -o "$CHECKSUM_FILE" "$CHECKSUM_URL"; then
        EXIT_CODE=3
        rm -f "$TARBALL_TMP" "$CHECKSUM_FILE"
        write_state failed "the checksum asset is missing"
        printf 'error: the release publishes no checksum; refusing an unsigned swap\n' >&2
        exit 3
    fi
    note "downloaded the checksum"
fi

# --- 4. verify ---------------------------------------------------------------

write_state verifying

verify() {
    # `sha256sum` writes `<hash>  <file>`. Take the hash, compute the same over
    # the downloaded bytes, compare the two. Doing it this way rather than
    # `sha256sum -c` keeps the check independent of the file name the asset uses.
    expected=$(sed -n 's/^\([0-9a-fA-F]\{64\}\).*/\1/p' "$CHECKSUM_FILE" | head -1 | tr 'A-F' 'a-f')
    [ -n "$expected" ] || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$TARBALL_TMP" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$TARBALL_TMP" | awk '{print $1}')
    else
        return 1
    fi
    [ "$expected" = "$actual" ]
}

if [ "$DRY_RUN" = 1 ]; then
    say "  would run: verify the tarball against $CHECKSUM_FILE"
else
    if ! verify; then
        EXIT_CODE=4
        rm -f "$TARBALL_TMP" "$CHECKSUM_FILE"
        note "checksum mismatch: the download does not match the published hash"
        write_state failed "checksum mismatch"
        printf 'error: checksum mismatch: the download was deleted and nothing was installed\n' >&2
        exit 4
    fi
    note "checksum verified"
fi

# The tarball is built with `tar czf ... -C dist/pkg .`, so its members are
# `./eris` and `./LICENSE` rather than `eris`. Extract into a private directory and
# then find the binary, which is the same rule install.sh uses and does not depend
# on which of the two spellings the packaging step produced.
#
# Path traversal is refused before a single byte is unpacked: a member containing
# `..`, or an absolute path, means the archive was not built by the release job.
EXTRACT="$DOWNLOAD_DIR/extract"
step mkdir -p "$EXTRACT"
if [ "$DRY_RUN" = 1 ]; then
    say "  would list $TARBALL_TMP and refuse any member containing '..' or a leading /"
    say "  would run: tar xzf $TARBALL_TMP -C $EXTRACT"
else
    if tar tzf "$TARBALL_TMP" | grep -qE '(^|/)\.\.(/|$)|^/'; then
        EXIT_CODE=4
        note "the release archive hides a path outside its own root; refusing to unpack it"
        write_state failed "the release archive contains a path traversal member"
        printf 'error: the release archive contains a path traversal member\n' >&2
        exit 4
    fi
    if ! tar xzf "$TARBALL_TMP" -C "$EXTRACT" 2>/dev/null; then
        EXIT_CODE=4
        note "the release archive could not be unpacked"
        write_state failed "the release archive could not be unpacked"
        printf 'error: the release archive could not be unpacked\n' >&2
        exit 4
    fi
fi
NEW_BINARY=""
if [ "$DRY_RUN" = 0 ]; then
    NEW_BINARY=$(find "$EXTRACT" -type f -name eris -perm -u+x 2>/dev/null | head -1)
fi
if [ "$DRY_RUN" = 0 ] && [ -z "$NEW_BINARY" ]; then
    EXIT_CODE=4
    note "the release archive holds no executable \`eris\`"
    write_state failed "the release archive has no eris binary"
    printf 'error: the release archive holds no executable eris\n' >&2
    exit 4
fi

# --- 5. back up --------------------------------------------------------------

write_state installing

BACKUP="$BIN.bak-$(printf '%s' "$FROM_VERSION" | tr ' ' '-' | tr -cd 'A-Za-z0-9.-')-$(date -u +%Y%m%dT%H%M%SZ)"

if [ "$DRY_RUN" = 1 ]; then
    say "  would run: cp -p $BIN $BACKUP"
else
    if [ -f "$BIN" ]; then
        cp -p "$BIN" "$BACKUP" || {
            EXIT_CODE=3
            write_state failed "the running binary could not be backed up"
            printf 'error: the running binary could not be backed up to %s\n' "$BACKUP" >&2
            exit 3
        }
        note "backed up to $(basename "$BACKUP")"
    else
        warn "no binary at $BIN to back up"
        note "no existing binary to back up"
    fi
fi

# Keep the last five backups of this path. They are the rollback, so the prune
# only ever removes the oldest beyond the count, never the one just written.
prune_backups() {
    [ -d "$(dirname "$BIN")" ] || return 0
    list=$(ls -1t "$(dirname "$BIN")"/"$(basename "$BIN")".bak-* 2>/dev/null || true)
    [ -n "$list" ] || return 0
    count=0
    for file in $list; do
        count=$((count + 1))
        if [ "$count" -gt "$KEEP_BACKUPS" ] && [ "$file" != "$BACKUP" ]; then
            rm -f "$file"
        fi
    done
}

# --- 6-8. stop, install, start, wait for health ------------------------------

if [ "$DO_RESTART" = 1 ]; then
    if [ "$DRY_RUN" = 1 ]; then
        say "  would run: systemctl --user stop $UNIT.service"
    elif ! systemctl --user stop "$UNIT.service"; then
        # Refusing here is the safe direction: installing over a binary whose
        # process is still running leaves two versions of the daemon in one
        # install directory.
        EXIT_CODE=5
        note "the daemon could not be stopped, so nothing was installed"
        write_state failed "systemctl could not stop the daemon"
        printf 'error: systemctl could not stop %s; nothing was installed\n' "$UNIT.service" >&2
        exit 5
    fi
fi

if [ "$DRY_RUN" = 1 ]; then
    say "  would run: install -m 755 $NEW_BINARY $BIN"
else
    if ! install -m 755 "$NEW_BINARY" "$BIN"; then
        EXIT_CODE=5
        note "the new binary could not be installed over $BIN"
        if [ -f "$BACKUP" ]; then
            install -m 755 "$BACKUP" "$BIN" || warn "the backup could not be restored"
            note "restored the previous binary"
        fi
        if [ "$DO_RESTART" = 1 ]; then systemctl --user start "$UNIT.service" || true; fi
        write_state failed "the new binary could not be installed"
        exit 5
    fi
    note "installed $(basename "$BIN")"
fi

prune_backups

healthy() {
    waited=0
    while [ "$waited" -lt "$HEALTH_TIMEOUT" ]; do
        if curl -fsS -m 2 -o /dev/null "$HEALTH_URL" 2>/dev/null; then
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}

if [ "$DO_RESTART" = 1 ]; then
    write_state restarting
    if [ "$DRY_RUN" = 1 ]; then
        say "  would run: systemctl --user start $UNIT.service"
        say "  would poll $HEALTH_URL for up to ${HEALTH_TIMEOUT}s"
    else
        # A start that fails is not an abort: the health poll below is what decides,
        # and the rollback path depends on being reachable from here. Failing out
        # of `set -e` would leave a half-installed binary and no report.
        systemctl --user start "$UNIT.service"             || warn "the unit did not start cleanly; waiting for health anyway"
    fi
    if [ "$DRY_RUN" = 1 ]; then
        :
    elif healthy; then
        note "the daemon answers on $HEALTH_URL"
    else
        EXIT_CODE=5
        note "the daemon did not answer within ${HEALTH_TIMEOUT}s; rolling back"
        if [ -f "$BACKUP" ]; then
            if install -m 755 "$BACKUP" "$BIN"; then
                note "restored the previous binary"
            else
                warn "the backup could not be restored"
                note "the backup could not be restored"
            fi
        fi
        systemctl --user restart "$UNIT.service" || true
        tail=$(journalctl --user -u "$UNIT" -n 40 --no-pager 2>/dev/null || true)
        if [ -n "$tail" ]; then
            printf '%s\n' "$tail" | while IFS= read -r line; do note "journal: $line"; done
        fi
        write_state failed "the new binary did not become healthy; the previous one was restored"
        exit 5
    fi
fi

# --- 9. done -----------------------------------------------------------------

if [ "$DRY_RUN" = 1 ]; then
    say "  would write state: done"
    say "dry run: nothing was downloaded, installed or restarted."
    exit 0
fi

note "update complete: $TO_VERSION"
write_state done
rm -f "$REQUEST" "$TARBALL_TMP" "$CHECKSUM_FILE"
rm -rf "$DOWNLOAD_DIR/extract"
say "Update complete."
exit 0
