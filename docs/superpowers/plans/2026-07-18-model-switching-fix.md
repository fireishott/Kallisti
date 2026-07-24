# Model Switching Fix, Models List Bugs, Wallpaper Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix model switching (currently non-functional — dispatches through a chat path that never reaches the command dispatcher), fix real bugs in `models.list` (dropped list-format providers, missing dynamic-catalog models, wrong context-window resolution), add pull-to-refresh, and replace the placeholder wallpaper default.

**Architecture:** Connector gains its first config-mutating RPC (`model.set`, read-modify-write on `config.yaml`). Relay gets a passthrough write endpoint. iOS switches from chat-message dispatch + reply-text regex parsing to a direct RPC call with a typed response.

**Tech Stack:** Swift/SwiftUI (iOS), Python/FastAPI (relay), Python (connector)

## Global Constraints

- Model switching is global-default only (equivalent to `/model <name> --global`); no session-scoped override
- `model.set` RPC must read-modify-write config.yaml without destroying existing structure/comments (use ruamel.yaml round-trip if available, matching the existing fallback-import pattern in client.py)
- Write endpoints propagate errors (don't swallow) — matches the cron CRUD pattern
- Dynamic catalog matching is scoped to the single configured provider only, never a full multi-provider dump
- No more regex-parsing of chat reply text for state confirmation — RPC responses are the source of truth

---

## Task 1: Connector — model.set RPC

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py`

**Interfaces:**
- Produces: `model.set` RPC method, dispatched via `_handle_rpc_request()`
- Consumes: `~/.hermes/config.yaml`

- [ ] **Step 1: Add _rpc_model_set method**

Add after `_rpc_models_list()`:

```python
async def _rpc_model_set(self, params: dict) -> dict:
    """Set the global default model in ~/.hermes/config.yaml.

    This is equivalent to running `/model <name> --global` in the TUI —
    it edits the persistent default, not a session-scoped override.
    """
    hermes_home = self._resolve_hermes_home()
    config_path = hermes_home / "config.yaml"
    name = params.get("name")
    provider = params.get("provider")
    if not name or not provider:
        raise RuntimeError("model.set requires 'name' and 'provider'")

    if not config_path.is_file():
        raise RuntimeError("config.yaml not found")

    try:
        from ruamel.yaml import YAML
        yaml_engine = YAML()
        yaml_engine.preserve_quotes = True
        with open(config_path, "r", encoding="utf-8") as f:
            config = yaml_engine.load(f) or {}
    except ImportError:
        import yaml as yaml_engine
        with open(config_path, "r", encoding="utf-8") as f:
            config = yaml_engine.safe_load(f) or {}

    if "model" not in config or not isinstance(config.get("model"), dict):
        config["model"] = {}

    config["model"]["default"] = name
    config["model"]["provider"] = provider

    # If the target provider declares a base_url, mirror it onto the
    # top-level model.base_url so context-window resolution and other
    # base_url-dependent lookups stay consistent with the new default.
    providers = config.get("providers")
    if isinstance(providers, dict) and provider in providers:
        provider_entry = providers[provider]
        if isinstance(provider_entry, dict) and provider_entry.get("base_url"):
            config["model"]["base_url"] = provider_entry["base_url"]

    with open(config_path, "w", encoding="utf-8") as f:
        if hasattr(yaml_engine, "dump"):
            yaml_engine.dump(config, f)
        else:
            yaml_engine.dump(config, f)

    return {"activeModel": self._read_active_model(hermes_home)}
```

- [ ] **Step 2: Add dispatch entry**

In `_handle_rpc_request()`, add to the if/elif chain (after the cron entries):

```python
elif method == "model.set":
    result = await self._rpc_model_set(params)
```

- [ ] **Step 3: Verify connector imports**

Run: `cd ~/Hermes-iOS/connector && python -c "from hermes_mobile_connector.client import HermesMobileConnector; print('OK')"`

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "feat(connector): add model.set RPC to edit the global default model"
```

---

## Task 2: Relay — POST /v1/model

**Files:**
- Modify: `relay/app/main.py`
- Modify: `relay/app/schemas.py`

**Interfaces:**
- Consumes: `send_connector_rpc()` with method `model.set`
- Produces: `POST /v1/model` endpoint

- [ ] **Step 1: Add ModelSetRequest schema**

In `relay/app/schemas.py`, add:

```python
class ModelSetRequest(BaseModel):
    name: str
    provider: str
```

- [ ] **Step 2: Add POST /v1/model endpoint**

In `relay/app/main.py`, add near the existing `GET /v1/models` route:

```python
@app.post("/v1/model")
async def set_active_model(
    body: ModelSetRequest,
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id,
            method="model.set",
            params=body.model_dump(),
            timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException as exc:
        raise exc
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc))
```

Add `ModelSetRequest` to the schemas import block at the top of `main.py`.

- [ ] **Step 3: Verify relay starts**

Run: `cd ~/Hermes-iOS/relay && python -c "from app.main import create_app; app = create_app(); print('OK')"`

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add relay/app/main.py relay/app/schemas.py
git commit -m "feat(relay): add POST /v1/model endpoint for real model switching"
```

---

## Task 3: iOS — ModelStore.switchModel + ModelSelectorSheet wiring

**Files:**
- Modify: `HermesMobile/Stores/ModelStore.swift`
- Modify: `HermesMobile/Features/Chat/ModelSelectorSheet.swift`
- Modify: `HermesMobile/Features/Chat/ChatScreen.swift`
- Modify: `HermesMobile/Stores/ChatStore.swift`

**Interfaces:**
- Consumes: `POST /v1/model` via `RelayAPIClient`
- Produces: `ModelStore.switchModel(to:provider:)`, removes chat-dispatch model switching

- [ ] **Step 1: Add switchModel to ModelStore**

In `ModelStore.swift`, add:

```swift
struct ModelSetResponse: Decodable {
    let activeModel: ActiveModel?
}

func switchModel(to name: String, provider: String) async throws {
    guard let token = await accessTokenProvider() else {
        errorMessage = "Not connected to a relay."
        return
    }
    let body = ["name": name, "provider": provider]
    let response: ModelSetResponse = try await apiClient.post(
        path: "model", body: body, accessToken: token
    )
    if let updated = response.activeModel {
        self.activeModel = updated
    }
}
```

Check the exact property names for `apiClient`/`accessTokenProvider`/`activeModel` against the existing `ModelStore.swift` — match its established naming exactly (this plan's earlier tasks showed ModelStore uses `apiClient: RelayAPIClient?` and `accessTokenProvider: () async -> String?`, follow that).

- [ ] **Step 2: Update ModelSelectorSheet to call switchModel directly**

Remove any callback that dispatches a chat message. The `onSelect` callback (or equivalent) should call:

```swift
Task {
    do {
        try await modelStore.switchModel(to: model.name, provider: model.provider)
        dismiss()
    } catch {
        // show inline error, keep sheet open
        switchError = error.localizedDescription
    }
}
```

- [ ] **Step 3: Remove detectModelSwitch from ChatStore**

In `ChatStore.swift`, remove the `detectModelSwitch(in:)` method and its call site in the `.finished` case — it parsed reply text (`"Model switched to..."`) that the real command handler never produces through the connector's chat path, since `/model` sent as chat text was always misrouted. Model switching no longer flows through chat at all.

- [ ] **Step 4: Update ChatScreen's model chip switch handler**

Find where `ChatScreen.swift` previously called `chatStore.sendMessage("/model \(name)")` for switching (from the original model selector implementation) and replace with a call to `modelStore.switchModel(to:provider:)`.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Stores/ModelStore.swift HermesMobile/Features/Chat/ModelSelectorSheet.swift HermesMobile/Features/Chat/ChatScreen.swift HermesMobile/Stores/ChatStore.swift
git commit -m "feat(ios): switch models via direct RPC instead of chat-message dispatch"
```

---

## Task 4: Connector — models.list bug fixes (list-format, context window, cache key)

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py`

**Interfaces:**
- Modifies: `_read_available_models()`, `_context_length_from_config()`, `_context_window_for()`, `_cached_context_window()`

- [ ] **Step 1: Fix _read_available_models to handle list-format models**

In `_collect()` inside `_read_available_models()`, change:

```python
def _collect(provider_key: str, provider: dict) -> None:
    provider_models = provider.get("models")
    if not isinstance(provider_models, dict):
        return
    provider_name = provider.get("name") or str(provider_key)
    default_model = provider.get("default_model") or provider.get("model")
    for model_name, model_config in provider_models.items():
        ...
```

To:

```python
def _collect(provider_key: str, provider: dict) -> None:
    provider_models = provider.get("models")
    provider_name = provider.get("name") or str(provider_key)
    default_model = provider.get("default_model") or provider.get("model")

    if isinstance(provider_models, dict):
        for model_name, model_config in provider_models.items():
            context_length = None
            if isinstance(model_config, dict):
                try:
                    raw_length = model_config.get("context_length")
                    context_length = int(raw_length) if raw_length is not None else None
                except (TypeError, ValueError):
                    context_length = None
            models.append(
                {
                    "name": str(model_name),
                    "provider": str(provider_key),
                    "providerName": str(provider_name),
                    "contextWindow": context_length,
                    "isProviderDefault": model_name == default_model,
                }
            )
    elif isinstance(provider_models, list):
        # List form: bare model name strings. Context length, if present,
        # is a provider-level sibling key applying to every model in the list.
        provider_context_length = None
        try:
            raw_length = provider.get("context_length")
            provider_context_length = int(raw_length) if raw_length is not None else None
        except (TypeError, ValueError):
            provider_context_length = None
        for model_name in provider_models:
            models.append(
                {
                    "name": str(model_name),
                    "provider": str(provider_key),
                    "providerName": str(provider_name),
                    "contextWindow": provider_context_length,
                    "isProviderDefault": model_name == default_model,
                }
            )
```

- [ ] **Step 2: Fix _context_length_from_config to handle list-format models**

Find `_context_length_from_config()` and apply the same dict/list handling — when `provider_models` is a list, return the provider-level `context_length` if the target model name is in the list.

- [ ] **Step 3: Fix _context_window_for to pass base_url and provider**

Find `_context_window_for()`. Current signature only takes `model_name` and `hermes_home`. Add `base_url` and `provider` parameters, and update the subprocess invocation:

```python
def _context_window_for(
    model_name: str,
    hermes_home: Path | None = None,
    base_url: str | None = None,
    provider: str | None = None,
) -> int:
    ...
    script = (
        f"from agent.model_metadata import get_model_context_length; "
        f"print(get_model_context_length({model_name!r}, base_url={base_url!r}, provider={provider!r}))"
    )
    ...
```

Update the fallback value in the `except Exception:` branch from `128_000` to `256_000` (matching hermes-agent's real documented final fallback).

Update every call site of `_context_window_for()` to pass `base_url`/`provider` through from whatever context they already have available (the calling function already has these values per the earlier investigation — thread them through, don't reintroduce the gap).

- [ ] **Step 4: Fix cache key in _cached_context_window**

Find `_cached_context_window()`. Currently builds the cache key using only the top-level `model.base_url`. Add a `base_url` parameter to the function signature (instead of reaching for a module/config-level default internally) and require callers to pass the correct per-provider `base_url` explicitly:

```python
def _cached_context_window(hermes_home: Path, model_name: str, base_url: str | None) -> int | None:
    cache_path = hermes_home / "context_length_cache.yaml"
    ...
    cache_key = f"{model_name}@{base_url}"
    ...
```

(This function already takes `base_url` as a parameter per the original code — the bug is that callers pass the wrong value. Fix call sites to pass the provider-specific `base_url`, not the top-level default.)

- [ ] **Step 5: Verify connector imports**

Run: `cd ~/Hermes-iOS/connector && python -c "from hermes_mobile_connector.client import HermesMobileConnector; print('OK')"`

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "fix(connector): handle list-format model providers, fix context-window resolution and cache key"
```

---

## Task 5: Connector — Dynamic catalog models

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py`

**Interfaces:**
- Produces: `_read_dynamic_catalog_models()` helper
- Consumed by: `_rpc_models_list()`

- [ ] **Step 1: Add _read_dynamic_catalog_models function**

Add near `_read_available_models`:

```python
def _read_dynamic_catalog_models(hermes_home: Path, config: dict) -> list[dict]:
    """Surface models from the dynamic model catalog cache for the
    currently configured provider (e.g. OpenRouter via model.provider=auto).

    Scoped to the single configured provider only — the cache contains
    100+ gateway providers, and dumping all of them would flood the
    picker with irrelevant duplicates.
    """
    cache_path = hermes_home / "models_dev_cache.json"
    if not cache_path.is_file():
        return []
    try:
        with open(cache_path, "r", encoding="utf-8") as f:
            catalog = json.load(f)
    except Exception:
        return []
    if not isinstance(catalog, dict):
        return []

    model_cfg = config.get("model")
    if not isinstance(model_cfg, dict):
        return []
    provider_id = model_cfg.get("provider")
    base_url = model_cfg.get("base_url") or ""

    matched_provider_key = None
    if provider_id and provider_id != "auto" and provider_id in catalog:
        matched_provider_key = provider_id
    elif base_url:
        for key in catalog:
            if key in base_url:
                matched_provider_key = key
                break

    if not matched_provider_key:
        return []

    provider_entry = catalog.get(matched_provider_key)
    if not isinstance(provider_entry, dict):
        return []
    provider_models = provider_entry.get("models")
    if not isinstance(provider_models, dict):
        return []

    default_model = model_cfg.get("default")
    results: list[dict] = []
    for model_id, model_meta in provider_models.items():
        if not isinstance(model_meta, dict):
            continue
        limit = model_meta.get("limit")
        context_limit = None
        if isinstance(limit, dict):
            raw_context = limit.get("context")
            if isinstance(raw_context, (int, float)):
                context_limit = int(raw_context)
        results.append({
            "name": str(model_id),
            "provider": str(matched_provider_key),
            "providerName": str(provider_entry.get("name", matched_provider_key)),
            "contextWindow": context_limit,
            "isProviderDefault": model_id == default_model,
        })
    return results
```

Add `import json` at the top of `client.py` if not already present (check first — the file already parses JSON elsewhere for job results, so it's likely already imported).

- [ ] **Step 2: Wire into _rpc_models_list**

Modify `_rpc_models_list()` to merge in dynamic catalog models, deduplicating by `(name, provider)` pair in case a model appears in both config.yaml and the catalog:

```python
def _rpc_models_list(self) -> dict:
    hermes_home = self._resolve_hermes_home()
    config_path = hermes_home / "config.yaml"
    config: dict = {}
    if config_path.is_file():
        try:
            with open(config_path, "r", encoding="utf-8") as f:
                import yaml
                config = yaml.safe_load(f) or {}
        except Exception:
            config = {}

    models = self._read_available_models(hermes_home)
    dynamic_models = _read_dynamic_catalog_models(hermes_home, config)

    seen = {(m["name"], m["provider"]) for m in models}
    for m in dynamic_models:
        key = (m["name"], m["provider"])
        if key not in seen:
            models.append(m)
            seen.add(key)

    logger.info("models.list RPC: hermes_home=%s, models_count=%d", hermes_home, len(models))
    return {
        "activeModel": self._read_active_model(hermes_home),
        "models": models,
    }
```

- [ ] **Step 3: Verify connector imports**

Run: `cd ~/Hermes-iOS/connector && python -c "from hermes_mobile_connector.client import HermesMobileConnector; print('OK')"`

- [ ] **Step 4: Manual smoke test against real config**

Run a small script to confirm the fix surfaces the user's actual active model:

```bash
cd ~/Hermes-iOS/connector && python -c "
from pathlib import Path
from hermes_mobile_connector.client import _read_dynamic_catalog_models
import yaml
hermes_home = Path.home() / '.hermes'
with open(hermes_home / 'config.yaml') as f:
    config = yaml.safe_load(f)
models = _read_dynamic_catalog_models(hermes_home, config)
print(f'Found {len(models)} dynamic models')
for m in models[:5]:
    print(m)
"
```

Expected: non-empty list, including something matching `anthropic/claude-opus-4.6` if the user's config still points at OpenRouter.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "feat(connector): surface dynamic model catalog entries in models.list"
```

---

## Task 6: iOS — Pull-to-Refresh

**Files:**
- Modify: `HermesMobile/Features/Chat/ModelSelectorSheet.swift`

**Interfaces:**
- Consumes: `ModelStore.loadModels(forceRefresh:)` (already exists)

- [ ] **Step 1: Add refreshable modifier**

In `ModelSelectorSheet.swift`, add to the `List`:

```swift
.refreshable {
    await modelStore.loadModels(forceRefresh: true)
}
```

Check the exact method name/signature against the existing `ModelStore.swift` (it may be `loadModels(force:)` or `loadModels(forceRefresh:)` — match whatever's actually there).

- [ ] **Step 2: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ModelSelectorSheet.swift
git commit -m "feat(ios): add pull-to-refresh to model selector"
```

---

## Task 7: iOS — Wallpaper Default Cleanup

**Files:**
- Modify: `HermesMobile/Core/Theme.swift`

**Interfaces:**
- Modifies: `ChatWallpaperBackground.defaultBackground`

- [ ] **Step 1: Replace placeholder with theme-tinted gradient**

In `Theme.swift`, find `ChatWallpaperBackground.defaultBackground` (currently a `hexagon.fill` SF Symbol placeholder) and replace with:

```swift
@ViewBuilder
private var defaultBackground: some View {
    RadialGradient(
        colors: [tint.opacity(0.06), Color(.systemBackground)],
        center: .center,
        startRadius: 0,
        endRadius: 500
    )
}
```

Remove the code comment referencing a future "human designer drops in a logo silhouette" plan — this is now the permanent default.

- [ ] **Step 2: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Core/Theme.swift
git commit -m "fix(ios): replace placeholder wallpaper default with theme-tinted gradient"
```

---

## Task 8: Deploy and Verify

- [ ] **Step 1: Push to GitHub**

```bash
cd ~/Hermes-iOS && git push origin master
```

- [ ] **Step 2: Sync and rebuild relay**

```bash
ssh fihadmin@INTERNAL_HOST "cd /home/agent/hermes-ios-work && git pull origin master"
ssh fihadmin@INTERNAL_HOST "cp /home/agent/hermes-ios-work/relay/app/main.py /home/agent/Hermes-iOS/relay/app/main.py && cp /home/agent/hermes-ios-work/relay/app/schemas.py /home/agent/Hermes-iOS/relay/app/schemas.py"
ssh fihadmin@INTERNAL_HOST "cd /home/agent/deploy/hermes-relay && docker compose down && docker compose up -d --build"
```

Verify: `curl -s http://localhost:8010/v1/health` from the ignyte host.

- [ ] **Step 3: Restart connector**

```bash
ssh fihadmin@INTERNAL_HOST "cp -r /home/agent/hermes-ios-work/connector/src /home/agent/Hermes-iOS/connector/"
ssh fihadmin@INTERNAL_HOST "systemctl --user restart hermes-mobile-connector.service"
```

Verify: `systemctl --user status hermes-mobile-connector.service` shows active/running.

- [ ] **Step 4: Manual RPC smoke test**

```bash
ssh fihadmin@INTERNAL_HOST "CREDS=\$(cat /home/agent/.hermes/profiles/ignyte/home/.hermes-mobile/state.json | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"connector_credential\"])') && curl -s http://localhost:8010/v1/models -H \"Authorization: Bearer \$CREDS\""
```

Expected: response includes models from config.yaml AND dynamic catalog (should now be non-empty for the active provider).

- [ ] **Step 5: Build and install iOS app**

Build on MBP (entitlements-strip + keychain-unlock pattern), install on iPhone and iPad via `xcrun devicectl device install app`.

- [ ] **Step 6: Manual verification on device**

- Open Model Selector, confirm more models appear than before (including the dynamic-catalog ones)
- Pull to refresh, confirm it re-fetches
- Tap a different model, confirm it switches (check for a confirmation, not an error)
- Confirm the active model chip in the toolbar updates to reflect the new selection
- Check chat wallpaper default — should show a subtle gradient, not a hexagon icon

- [ ] **Step 7: Final commit**

```bash
cd ~/Hermes-iOS
git commit --allow-empty -m "chore: verify model switching fix, models.list bugs, wallpaper cleanup end-to-end"
```
