#!/bin/sh
# Craft Anything updater: keep the installed CLI on the newest upstream release.
#
#   1. ask GitHub for the newest release tag
#   2. stop when the installed CLI already is that version
#   3. download the release tarball, the signed checksum list and the signing key
#   4. verify the checksum list's GPG signature against a pinned fingerprint
#   5. verify the tarball's SHA256 against that signed list
#   6. install the new CLI, keeping the old one as a backup
#   7. run doctor, and restore the backup when the new CLI is not healthy
#   8. refresh the version and the tool counts in the shipped skill
#
# Nothing here needs root and nothing writes outside $HOME.
#
# Flags:
#   --force      reinstall even when the version already matches
#   --dry-run    report what would happen and change nothing
#   -h, --help   this text

set -eu

REPO="storytold/craft-anything"
API="https://api.github.com/repos/$REPO/releases/latest"
RELEASE_BASE="https://github.com/$REPO/releases/download"
EXPECTED_FINGERPRINT="2B76741B414AAD6CD33E631213A005C0B9D7C3ED"

CLI_BIN="${CRAFT_CLI_BIN:-$HOME/.local/bin/craft-anything-cli}"
WORK="${CRAFT_UPDATE_DIR:-$HOME/.cache/craft-anything-update}"
STATE="${CRAFT_UPDATE_STATE:-$HOME/.local/share/craft-anything-update}"
CLONE="${CRAFT_CLONE:-$HOME/research/storytold/craft-anything}"
SKILL_REPO="${CRAFT_SKILL_REPO:-$HOME/src/kallisti/skills/craft-anything/SKILL.md}"
SKILL_LIVE="${CRAFT_SKILL_LIVE:-$HOME/.config/kallisti/skills/craft-anything/SKILL.md}"

FORCE=0
DRY=0

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    --dry-run) DRY=1 ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    *) die "unknown flag: $1" ;;
  esac
  shift
done

mkdir -p "$WORK" "$STATE"
STAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

CURRENT=""
if [ -x "$CLI_BIN" ]; then
  CURRENT=$("$CLI_BIN" --version 2>/dev/null | awk '{print $NF}' | tr -d 'v')
fi

say "checking the newest $REPO release"
LATEST=$(curl -fsSL --retry 3 "$API" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])') || die "cannot read the release list from $API"
LATEST=$(printf '%s' "$LATEST" | tr -d 'v')
say "installed: ${CURRENT:-none}  newest: $LATEST"

if [ "$CURRENT" = "$LATEST" ] && [ "$FORCE" -eq 0 ]; then
  say "already on $LATEST, nothing to do"
  printf '{"version":"%s","checked_at":"%s"}\n' "$LATEST" "$STAMP" > "$STATE/state.json"
  exit 0
fi

case "$(uname -m)" in
  x86_64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) die "upstream ships no build for $(uname -m)" ;;
esac
ASSET="craft-anything-$LATEST-linux-$ARCH.tar.gz"

cd "$WORK"
fetch() {
  curl -fsSL --retry 3 -o "$2" "$1" || die "download failed: $1"
}
fetch "$RELEASE_BASE/v$LATEST/$ASSET" "$ASSET"
fetch "$RELEASE_BASE/v$LATEST/SHA256SUMS.txt" SHA256SUMS.txt
fetch "$RELEASE_BASE/v$LATEST/SHA256SUMS.txt.asc" SHA256SUMS.txt.asc
fetch "$RELEASE_BASE/v$LATEST/release-signing-key.asc" release-signing-key.asc
say "downloaded $ASSET"

GNUPGHOME="$WORK/gnupg"
rm -rf "$GNUPGHOME"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
gpg --batch --no-tty --homedir "$GNUPGHOME" --import release-signing-key.asc >/dev/null 2>&1 || die "the release signing key did not import"
FPR=$(gpg --batch --no-tty --homedir "$GNUPGHOME" --with-colons --fingerprint 2>/dev/null | awk -F: '/^fpr:/ {print $10; exit}')
if [ "$FPR" != "$EXPECTED_FINGERPRINT" ]; then
  die "the release key fingerprint is ${FPR:-none}, not the pinned $EXPECTED_FINGERPRINT"
fi
gpg --batch --no-tty --homedir "$GNUPGHOME" --verify SHA256SUMS.txt.asc SHA256SUMS.txt >/dev/null 2>&1 || die "the checksum list is not signed by that key"
say "checksum list carries a good signature from $FPR"

EXPECTED=$(awk -v a="$ASSET" '$2 == a {print $1}' SHA256SUMS.txt)
[ -n "$EXPECTED" ] || die "$ASSET is not in the signed checksum list"
ACTUAL=$(sha256sum "$ASSET" | awk '{print $1}')
[ "$EXPECTED" = "$ACTUAL" ] || die "checksum mismatch for $ASSET"
say "tarball checksum matches the signed list"

rm -rf "$WORK/extract"
mkdir -p "$WORK/extract"
tar xzf "$ASSET" -C "$WORK/extract"
NEW=$(find "$WORK/extract" -type f -name craft-anything-cli | head -1)
[ -n "$NEW" ] || die "the tarball carries no craft-anything-cli"
NEWVER=$("$NEW" --version 2>/dev/null | awk '{print $NF}' | tr -d 'v')
[ "$NEWVER" = "$LATEST" ] || die "the extracted CLI says ${NEWVER:-nothing}, not $LATEST"

if [ "$DRY" -eq 1 ]; then
  say "dry run: would install $NEWVER as $CLI_BIN"
  exit 0
fi

mkdir -p "$(dirname "$CLI_BIN")"
if [ -x "$CLI_BIN" ]; then
  mkdir -p "$STATE/backups"
  cp -p "$CLI_BIN" "$STATE/backups/craft-anything-cli-${CURRENT:-unknown}"
fi
cp -f "$NEW" "$CLI_BIN.new"
chmod 0755 "$CLI_BIN.new"
mv -f "$CLI_BIN.new" "$CLI_BIN"
say "installed $LATEST"

HEALTH=$("$CLI_BIN" doctor 2>&1 || true)
if printf '%s' "$HEALTH" | grep -q '"healthy":true'; then
  say "doctor reports healthy"
else
  warn "doctor did not report healthy; restoring the previous binary"
  if [ -f "$STATE/backups/craft-anything-cli-${CURRENT:-unknown}" ]; then
    cp -f "$STATE/backups/craft-anything-cli-${CURRENT:-unknown}" "$CLI_BIN"
    chmod 0755 "$CLI_BIN"
  fi
  printf '%s\n' "$HEALTH" | tail -5 >&2
  exit 1
fi

pkill -f "craft-anything-cli mcp" >/dev/null 2>&1 || true
say "stopped the running MCP child; the daemon starts the new binary on the next turn"

CLI_COUNT=$("$CLI_BIN" commands --format json 2>/dev/null | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))') || CLI_COUNT=""
python3 - "$LATEST" "$CLI_COUNT" "$SKILL_REPO" "$SKILL_LIVE" <<'PY'
import os, re, sys, urllib.request
version, cli_count = sys.argv[1], sys.argv[2]
paths = sys.argv[3:]
engine = toolsets = ""
try:
    url = "https://raw.githubusercontent.com/storytold/craft-anything/v%s/docs/tools.md" % version
    text = urllib.request.urlopen(url, timeout=30).read().decode("utf-8", "replace")
    match = re.search(r"\*\*(\d+) engine tools\*\* in (\d+) toolsets", text)
    if match:
        engine, toolsets = match.group(1), match.group(2)
except Exception as error:
    print("warning: could not read docs/tools.md: %s" % error)
for path in paths:
    if not os.path.exists(path):
        continue
    text = open(path, encoding="utf-8").read()
    before = text
    text = re.sub(r"\(v\d+\.\d+\.\d+, from the official GitHub release",
                  "(v%s, from the official GitHub release" % version, text)
    if cli_count:
        text = re.sub(r"\d+ CLI commands", "%s CLI commands" % cli_count, text)
    if engine and toolsets:
        text = re.sub(r"\d+ engine tools in\s*\d+\s*toolsets",
                      "%s engine tools in %s\ntoolsets" % (engine, toolsets), text)
    if text != before:
        open(path, "w", encoding="utf-8").write(text)
        print("refreshed %s" % path)
PY

printf '{"version":"%s","updated_at":"%s"}\n' "$LATEST" "$STAMP" > "$STATE/state.json"
say "state written to $STATE/state.json"

if [ -d "$CLONE/.git" ]; then
  git -C "$CLONE" fetch --tags --quiet origin 2>/dev/null || warn "could not refresh the source clone tags"
fi
