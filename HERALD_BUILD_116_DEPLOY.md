# Herald Build 116 - Deployment Marching Orders (for Claude Code)

**Author:** diagnostic pass on live production, 2026-08-02 22:xx PDT
**Severity:** P0 - chat is dead for every paired device; Talk is dead; session titles are garbage.
**Headline:** The P0 is **connector-only**. Deploying the connector fix un-bricks the **existing Build 115** install in place - **no new IPA / TestFlight build is required** to restore chat. An iOS Build 116 is optional hardening (§7).

---

## 0. TL;DR

1. **Chat "Could not reach the Herald host" (P0):** the connector's access-token validator is an in-memory `set` that is seeded at startup with **only the shared connector credential**. Every per-device `hd_…` token (minted at pairing, persisted in `device_registry.json`) is **not** rehydrated, so after **any** connector restart every device gets `401` on every call - including `/v1/auth/refresh`, which itself requires a valid token, so the app can never self-heal. This host restarts several times an hour (3× in the last 90 min), so it bricks constantly. **Fix: rehydrate persisted device tokens at startup + self-heal in `require_auth`.**
2. **Talk "MiMo API key was rejected by the upstream service" (P1):** there is **no server-side MiMo key** configured (`_load_mimo_api_key()` returns `None`; `/home/agent/.config/herald-mimo.env` does not exist). Talk falls back to a device-supplied key that upstream rejects. **Fix: set a valid server-side `HERALD_MIMO_API_KEY` (needs a current token-plan-sgp key from Curtis).**
3. **Session titles show `[System context: 2026-08-…]` (P2):** `_derived_title` uses the first `role='user'` row verbatim; the injected `[System context: <time>]` preamble is a user row and is not excluded (the one filter that tries uses a stale literal). **Fix: exclude the preamble in the title query.**

Fixes 1 and 3 are **portable** (the two files are byte-identical between the canonical repo and the live host). Fix for `require_auth` and the MiMo key are applied per-checkout / as ops config.

---

## 1. Environment / topology (verified live, do not trust older docs)

- **Production Hermes = `fih-ai-host` = `192.168.10.118`.** SSH: `ssh fihadmin@192.168.10.118` (key auth works).
- **The relay is sunset.** Do **not** reintroduce a standalone relay service. `hermes-relay.fihonline.net` is a **legacy DNS name only**: it is a Caddy vhost that terminates TLS and reverse-proxies straight to the connector's HTTP facade. `docs/PRODUCTION_ARCHITECTURE.md` is stale (still describes the relay hop + OpenAI Realtime) - ignore it.
- **Front door is healthy:** `https://hermes-relay.fihonline.net/v1/health → 200` (`via: 1.1 Caddy`). The Caddy container is `192.168.10.101` (so connector-log source `192.168.10.101` = the app's requests forwarded through Caddy, **not** a device).
- **Live connector:** `/home/agent/Hermes-iOS/connector`, branch `build30/remediation @ cfb93aa`, package version `0.9.3`.
  - **Installed EDITABLE** (`__editable__.herald_connector-0.9.3.pth → /home/agent/Hermes-iOS/connector/src`). Confirmed: `python -c "import herald_connector; print(herald_connector.__file__)"` → `…/connector/src/herald_connector/__init__.py`.
  - **⇒ Editing `connector/src/herald_connector/*.py` on the host and restarting the service IS live. No wheel rebuild.** (This supersedes any prior "src edits are a no-op / live code is the wheel" note - the topology is now an editable src install.)
- **Process manager:** user systemd unit `hermes-mobile-connector.service` (`systemctl --user`), `enabled`, `Linger=yes`, `XDG_RUNTIME_DIR=/run/user/1000` - so `systemctl --user …` works over SSH. Facade on `0.0.0.0:8010`.
- **Connector home (state lives here):** `HERMES_MOBILE_CONNECTOR_HOME=/home/agent/.hermes/profiles/ignyte/home/.hermes-mobile`
  - device tokens: `…/.hermes-mobile/device_registry.json` (24 tokens persisted)
  - **Do NOT confuse with `/home/agent/.hermes-mobile/` - that is a stale 2.0.x-era copy.**
- **Canonical repo (where Claude Code edits/commits):** `/Users/you/Herald` on the MBP, branch `build30/remediation @ 15b0a39`.
- **File parity (MBP ↔ host), md5:**
  - `client.py` - `4f57135ae197682db8bb880394fa4c02` on **both** → **identical, safe to copy**.
  - `session_store.py` - `92d2b6c391c7586f173bd27577b9f4bb` on **both** → **identical, safe to copy**.
  - `http_facade.py` - MBP `69f9410…` vs host `27b2225…` → **DIFFERENT. Do NOT copy this file. Edit each checkout in place by matching context.**

### Do-not-touch / footguns
- **Never run `herald setup`** (wipes `state.json`; different failure class).
- **Do not modify anything on the MBP gateway/.env** - server-side fix only.
- **Do not reintroduce the relay** in any form.

---

## 2. Evidence (so you can trust this without re-deriving it)

Read-only proof taken on the live connector:

```
paired device token (present in device_registry.json)  →  HTTP 401
shared connector_credential                            →  HTTP 200
```

- `AccessTokenValidator` (`http_facade.py:80`) is `self._tokens: set[str]`; `is_valid` = `token in self._tokens`. No persistence.
- Startup seeds it with only the credential - `client.py:883-886`:
  ```python
  from .http_facade import set_token_validator, AccessTokenValidator
  if state.connector_credential:
      validator = AccessTokenValidator({state.connector_credential})
      set_token_validator(validator)
  ```
- `refresh_auth` (`http_facade.py:~3506`) calls `await require_auth(request)` first, then echoes the token - so a device whose token was evicted can't refresh.
- `device_registry.json` holds 24 `hd_…` tokens (`record_pairing_device` → `_save_device_registry`), but nothing loads them back.
- Connector restart history tonight: `20:48`, `22:00`, `22:17` - each restart wiped the set and re-bricked every device. Live log still shows `GET /v1/hosts/current → 401` after the last restart.

---

## 3. P0 fix - rehydrate device tokens (REQUIRED)

Two changes. Together they make the validator correct at startup **and** self-healing per-request.

### 3a. `session_store.py` - add a token accessor (portable; identical file both sides)

Insert right after `_save_device_registry(...)` (≈ line 128):

```python
def all_device_tokens() -> list[str]:
    """Every auth token recorded at pairing/registration time.

    B116: used at connector startup to rehydrate the in-memory access-token
    validator so paired devices survive a connector restart. Without this the
    validator held only the shared connector credential and every per-device
    ``hd_`` token 401'd after any restart.
    """
    return list(_load_device_registry().get("tokens", {}).keys())
```

### 3b. `client.py:883-886` - seed the validator from credential + registry (portable; identical file both sides)

Replace:

```python
            from .http_facade import set_token_validator, AccessTokenValidator
            if state.connector_credential:
                validator = AccessTokenValidator({state.connector_credential})
                set_token_validator(validator)
```

with:

```python
            from .http_facade import set_token_validator, AccessTokenValidator
            # B116: the validator is an in-memory set. It used to be seeded with
            # ONLY the shared connector credential, so every per-device `hd_`
            # token minted at pairing (persisted in device_registry.json) was
            # invalid after any connector restart -> 401 on every call, and
            # refresh_auth (which routes through require_auth) could not recover,
            # so chat died with "Could not reach the Herald host." This host
            # restarts several times an hour, so it bricked constantly.
            seed: set[str] = set()
            if state.connector_credential:
                seed.add(state.connector_credential)
            try:
                from .session_store import all_device_tokens
                seed.update(all_device_tokens())
            except Exception:
                logger.exception("B116: failed to hydrate persisted device tokens")
            if seed:
                set_token_validator(AccessTokenValidator(seed))
                logger.info("B116: token validator seeded with %d token(s)", len(seed))
```

### 3c. `http_facade.py` `require_auth` - self-heal from the registry (edit EACH checkout in place; file differs)

Find this block (host ≈ line 109, MBP ≈ line 109 - **match by content, not line number**):

```python
async def require_auth(request: Request) -> str:
    """Validate the Bearer token. Raises 401 if invalid."""
    token = await _extract_token(request)
    if not token or not _default_validator.is_valid(token):
        raise HTTPException(status_code=401, detail="Invalid or missing access token")
    return token
```

Replace with:

```python
async def require_auth(request: Request) -> str:
    """Validate the Bearer token. Raises 401 if invalid."""
    token = await _extract_token(request)
    if token and _default_validator.is_valid(token):
        return token
    # B116: a token missing from the in-memory set but present in the persisted
    # device registry is a device that paired before the last restart. Re-admit
    # it instead of 401'ing (a 401 here also kills /v1/auth/refresh and bricks
    # the app). This is the belt-and-suspenders to the startup rehydration.
    if token:
        try:
            from .session_store import device_id_for_token
            if device_id_for_token(token):
                _default_validator.add_token(token)
                return token
        except Exception:
            pass
    raise HTTPException(status_code=401, detail="Invalid or missing access token")
```

> Either 3b or 3c alone fixes the P0; ship **both** (3b = correct/eager at boot, 3c = robust for tokens paired after boot). 3c also means **existing Build 115 devices recover on their very next request - no re-pair.**

### Optional hygiene (not required)
`device_registry.json` has 24 tokens including old `build103-*` test installs. Re-admitting them is fine for a self-hosted single-user system. If you want to prune, add a `pairedAt` age cap in `all_device_tokens()` - but do **not** block the P0 on it.

---

## 4. P1 fix - Talk MiMo key (config; needs a valid key from Curtis)

Root cause: `mimo_proxy._load_mimo_api_key()` reads `HERALD_MIMO_API_KEY` from `/home/agent/.config/herald-mimo.env`, which **does not exist**, and the key is not in the unit env either → returns `None`. Talk then depends on a device-sent key that `token-plan-sgp.xiaomimimo.com` rejects. Base URL is already correct (`HERALD_MIMO_BASE_URL=https://token-plan-sgp.xiaomimimo.com`).

**Fix (server-side key, makes Talk independent of device state):**

```bash
ssh fihadmin@192.168.10.118 'umask 177; printf "HERALD_MIMO_API_KEY=%s\n" "<VALID_TOKEN_PLAN_SGP_KEY>" > /home/agent/.config/herald-mimo.env'
```

Then restart (§5) and verify against upstream **before** declaring success:

```bash
ssh fihadmin@192.168.10.118 '/home/agent/Hermes-iOS/connector/.venv/bin/python - <<PY
import sys,json,urllib.request,urllib.error
sys.path.insert(0,"/home/agent/Hermes-iOS/connector/src")
from herald_connector.mimo_proxy import _load_mimo_api_key
k=_load_mimo_api_key(); print("key loaded:", bool(k))
req=urllib.request.Request("https://token-plan-sgp.xiaomimimo.com/v1/chat/completions",
  data=json.dumps({"model":"MiMo-Audio","messages":[{"role":"user","content":"ping"}],"max_tokens":1}).encode(),
  headers={"Authorization":"Bearer "+(k or ""),"Content-Type":"application/json"})
try: print("UPSTREAM", urllib.request.urlopen(req,timeout=15).status)
except urllib.error.HTTPError as e: print("UPSTREAM", e.code, e.read()[:160].decode("utf-8","replace"))
PY'
```

**Blocker:** the current key is rejected/out-of-quota. **Curtis must supply a current token-plan-sgp key.** This step cannot be completed without it - flag and continue with the P0/P2.

---

## 5. P2 fix - session titles (portable; `session_store.py`)

`_derived_title` (`session_store.py:1104-1114`) takes the first `role='user'` message verbatim. The connector injects `[System context: {localTime}]` (`http_facade.py:2940`) as a user row, and the only exclusion filter (`session_store.py:583`) matches the stale literal `[System context - current local time]` (colon vs. that phrasing). Add exclusions to the `_derived_title` SELECT:

```sql
            SELECT content FROM messages
            WHERE session_id = ?
              AND role = 'user'
              AND content != ''
              AND active = 1
              AND content NOT LIKE '[System context:%'
              AND content NOT LIKE '[System context -%'
            ORDER BY timestamp ASC
            LIMIT 1
```

(Also update the stale literal at `session_store.py:583` to `'[System context:%'` for consistency.)

---

## 6. Deploy procedure (run from the MBP; Claude Code drives it)

### 6a. Patch the canonical repo + commit
```bash
cd /Users/you/Herald
# apply 3a, 3b, 3c(MBP http_facade.py by context), 5
# bump connector version for provenance:
#   connector/src/herald_connector/__init__.py  __version__ = "0.9.4"
#   connector/pyproject.toml                     version = "0.9.4"
git add connector/src/herald_connector/client.py \
        connector/src/herald_connector/session_store.py \
        connector/src/herald_connector/http_facade.py \
        connector/src/herald_connector/__init__.py connector/pyproject.toml
git commit -m "fix(b116): rehydrate device tokens on connector restart (un-brick chat); title + mimo-key hardening"
```

### 6b. Back up, deploy to the live host
```bash
H=fihadmin@192.168.10.118
D=/home/agent/Hermes-iOS/connector/src/herald_connector
TS=$(date +%Y%m%d-%H%M%S)
# backup
ssh $H "mkdir -p ~/b116-backup-$TS && cp $D/client.py $D/session_store.py $D/http_facade.py ~/b116-backup-$TS/"
# client.py + session_store.py are byte-identical base -> safe to copy from repo
scp connector/src/herald_connector/client.py        $H:$D/client.py
scp connector/src/herald_connector/session_store.py $H:$D/session_store.py
```
`http_facade.py` **differs** - do **not** scp it. Apply the §3c `require_auth` edit directly to the host file (e.g. via `ssh $H` + an in-place `python`/`patch`, matching the block in §3c).

### 6c. Restart + confirm the seed log
```bash
ssh fihadmin@192.168.10.118 \
  'systemctl --user restart hermes-mobile-connector.service && sleep 2 && \
   journalctl --user -u hermes-mobile-connector.service -n 40 --no-pager | grep -E "B116|listening|error|Traceback"'
```
Expect: `B116: token validator seeded with 25 token(s)` (24 device + 1 credential).

---

## 7. Verification (evidence before claiming success)

**Gate 1 - the exact bug is gone (a registry token must now 200):**
```bash
ssh fihadmin@192.168.10.118 '/home/agent/Hermes-iOS/connector/.venv/bin/python - <<PY
import json,urllib.request,urllib.error
reg=json.load(open("/home/agent/.hermes/profiles/ignyte/home/.hermes-mobile/device_registry.json"))
tok=next(iter(reg["tokens"]))
req=urllib.request.Request("http://127.0.0.1:8010/v1/hosts/current",headers={"Authorization":"Bearer "+tok})
try: print("registry token ->", urllib.request.urlopen(req,timeout=8).status)   # expect 200
except urllib.error.HTTPError as e: print("registry token ->", e.code)          # 401 = fix failed
PY'
```
**Gate 2 - front door:** `curl -s -o /dev/null -w "%{http_code}\n" https://hermes-relay.fihonline.net/v1/health` → `200`.
**Gate 3 - live app:** open the **existing Build 115** app (do **not** re-pair). Chat should start and reply; "Sup big guy" should send. Sessions list should show real titles, not `[System context: …]`.
**Gate 4 - Talk (only after §4 key is set):** the "MiMo API key was rejected" banner is gone and a Talk turn responds.

**Rollback:** `cp ~/b116-backup-$TS/* $D/ && systemctl --user restart hermes-mobile-connector.service`.

---

## 8. Scope notes

- **No iOS build is required for the P0/P2.** The connector fix restores Build 115 in place because the app still holds and sends its `hd_` token; once the connector re-admits it, the same requests succeed and `/v1/auth/refresh` unblocks the app automatically.
- **Optional iOS Build 116 hardening (defense-in-depth, separate change):** on a `401` from a previously-registered endpoint, have the app silently re-run device registration (`register_device`) before failing `ensureConversation`, instead of surfacing "Could not reach the Herald host." This would have masked the whole outage even with a broken connector. If you cut an iOS build, bump `CURRENT_PROJECT_VERSION → 116` only; keep `MARKETING_VERSION`.
- **The deeper systemic issue** is that a service which restarts several times an hour keeps auth state in memory. The §3 fix removes the dependency on uptime. Separately worth chasing (not part of B116): why the connector restarts so often (the host OOM-freezes; there's a `memory-limit.conf` drop-in with `max: 1G`).
