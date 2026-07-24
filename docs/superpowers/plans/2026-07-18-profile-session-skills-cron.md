# Profile Selector, Session History, Skills Browser, Cron Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Profile Selector, Skills Browser, Cron Manager features to Hermes iOS, polish session history UX, and wire up 6 new relay→connector RPC endpoints.

**Architecture:** Each new feature follows the proven model-selector pattern: iOS `@Observable` store → `RelayAPIClient` → relay passthrough RPC → connector WebSocket RPC → Hermes runtime. The relay is a thin proxy with graceful degradation. The connector reads local config/runtime and returns plain dicts.

**Tech Stack:** Swift/SwiftUI (iOS), Python/FastAPI (relay), Python/WebSocket (connector), SQLAlchemy (relay DB), PostgreSQL (production)

## Global Constraints

- All relay endpoints follow the `/v1/models` passthrough RPC pattern exactly
- All iOS stores follow `@Observable` + `RelayAPIClient` pattern from `ModelStore`
- Graceful degradation: relay returns empty defaults on connector failure, never 500s to client
- Profile switching uses chat-message dispatch (`/profile <name>`), not a dedicated API
- Cron connector implementation depends on Hermes CLI/API capabilities — shell out if needed
- Relay has NO migrations system — after schema changes, ALTER TABLE manually on production

---

## Task 1: Relay — Add 6 Passthrough RPC Endpoints

**Files:**
- Modify: `relay/app/main.py` — add 6 new route handlers
- Modify: `relay/app/schemas.py` — add Pydantic request models for cron CRUD

**Interfaces:**
- Consumes: `send_connector_rpc()` (existing), `get_auth_context` (existing), `success()` (existing)
- Produces: 6 new HTTP endpoints consumed by iOS `RelayAPIClient`

- [ ] **Step 1: Add Pydantic schemas for cron**

In `relay/app/schemas.py`, add:

```python
class CronCreateRequest(BaseModel):
    name: str
    schedule: str
    prompt: str

class CronUpdateRequest(BaseModel):
    name: str | None = None
    schedule: str | None = None
    prompt: str | None = None
    enabled: bool | None = None
```

- [ ] **Step 2: Add GET /v1/profiles endpoint**

In `relay/app/main.py`, add after the `/v1/models` route (around line 951):

```python
@app.get("/v1/profiles")
async def profile_catalog(
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id, method="profiles.list", timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException:
        return success({"profiles": [], "activeProfile": None})
    except Exception:
        return success({"profiles": [], "activeProfile": None})
```

- [ ] **Step 3: Add GET /v1/skills endpoint**

```python
@app.get("/v1/skills")
async def skill_catalog(
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id, method="skills.list", timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException:
        return success({"skills": []})
    except Exception:
        return success({"skills": []})
```

- [ ] **Step 4: Add GET /v1/cron endpoint**

```python
@app.get("/v1/cron")
async def cron_list(
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id, method="cron.list", timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException:
        return success({"jobs": []})
    except Exception:
        return success({"jobs": []})
```

- [ ] **Step 5: Add POST /v1/cron endpoint**

```python
@app.post("/v1/cron")
async def cron_create(
    body: CronCreateRequest,
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id,
            method="cron.create",
            params=body.model_dump(),
            timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException as exc:
        raise exc
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc))
```

- [ ] **Step 6: Add PATCH /v1/cron/{job_id} endpoint**

```python
@app.patch("/v1/cron/{job_id}")
async def cron_update(
    job_id: str,
    body: CronUpdateRequest,
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id,
            method="cron.update",
            params={"id": job_id, **body.model_dump()},
            timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException as exc:
        raise exc
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc))
```

- [ ] **Step 7: Add DELETE /v1/cron/{job_id} endpoint**

```python
@app.delete("/v1/cron/{job_id}")
async def cron_delete(
    job_id: str,
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id,
            method="cron.delete",
            params={"id": job_id},
            timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException as exc:
        raise exc
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc))
```

- [ ] **Step 8: Add GET /v1/memories endpoint**

```python
@app.get("/v1/memories")
async def memory_list(
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id, method="memories.list", timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException:
        return success({"memories": []})
    except Exception:
        return success({"memories": []})
```

- [ ] **Step 9: Add GET /v1/tools endpoint**

```python
@app.get("/v1/tools")
async def tool_list(
    auth: AuthContext = Depends(get_auth_context),
) -> dict:
    try:
        result = await send_connector_rpc(
            auth.user.id, method="tools.list", timeout_seconds=10.0,
        )
        return success(result)
    except HTTPException:
        return success({"tools": []})
    except Exception:
        return success({"tools": []})
```

- [ ] **Step 10: Verify relay starts without errors**

Run: `cd ~/Hermes-iOS/relay && python -c "from app.main import create_app; app = create_app(); print('OK')"`

Expected: `OK`

- [ ] **Step 11: Commit relay endpoints**

```bash
cd ~/Hermes-iOS
git add relay/app/main.py relay/app/schemas.py
git commit -m "feat(relay): add profiles, skills, cron, memories, tools passthrough RPC endpoints"
```

---

## Task 2: Connector — Add profiles.list RPC Handler

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py` — add `_rpc_profiles_list()` handler + dispatch

**Interfaces:**
- Consumes: `_resolve_hermes_home()` (existing), filesystem at `~/.hermes/profiles/`
- Produces: `{"activeProfile": {...}, "profiles": [...]}` dict returned via `rpc.response`

- [ ] **Step 1: Add _rpc_profiles_list method**

In `client.py`, add after the `_rpc_models_list` method (around line 1184):

```python
async def _rpc_profiles_list(self) -> dict:
    hermes_home = self._resolve_hermes_home()
    profiles_dir = os.path.join(hermes_home, "profiles")
    if not os.path.isdir(profiles_dir):
        return {"activeProfile": None, "profiles": []}

    # Read active profile from config
    active_name = None
    config_path = os.path.join(hermes_home, "config.yaml")
    if os.path.isfile(config_path):
        try:
            import yaml
            with open(config_path) as f:
                config = yaml.safe_load(f) or {}
            active_name = (config.get("profile") or {}).get("default")
        except Exception:
            pass

    profiles = []
    for entry in sorted(os.listdir(profiles_dir)):
        profile_path = os.path.join(profiles_dir, entry)
        if not os.path.isdir(profile_path):
            continue
        soul_path = os.path.join(profile_path, "SOUL.md")
        description = ""
        if os.path.isfile(soul_path):
            try:
                with open(soul_path) as f:
                    lines = f.readlines()
                # First non-empty, non-frontmatter line = description
                in_frontmatter = False
                for line in lines:
                    stripped = line.strip()
                    if stripped == "---":
                        in_frontmatter = not in_frontmatter
                        continue
                    if not in_frontmatter and stripped and not stripped.startswith("#"):
                        description = stripped
                        break
            except Exception:
                pass

        # Count skills
        skills_dir = os.path.join(profile_path, "skills")
        skill_count = len(os.listdir(skills_dir)) if os.path.isdir(skills_dir) else 0

        profiles.append({
            "name": entry,
            "description": description,
            "skillCount": skill_count,
        })

    active_profile = None
    if active_name:
        for p in profiles:
            if p["name"] == active_name:
                active_profile = dict(p)
                break

    return {"activeProfile": active_profile, "profiles": profiles}
```

- [ ] **Step 2: Add dispatch entry for profiles.list**

In `_handle_rpc_request()` (around line 977), add to the if/elif chain:

```python
elif method == "profiles.list":
    result = await self._rpc_profiles_list()
```

- [ ] **Step 3: Verify connector imports**

Run: `cd ~/Hermes-iOS/connector && python -c "from hermes_mobile_connector.client import HermesMobileConnector; print('OK')"`

Expected: `OK`

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "feat(connector): add profiles.list RPC handler"
```

---

## Task 3: Connector — Add skills.list RPC Handler

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py` — add `_rpc_skills_list()` handler + dispatch

**Interfaces:**
- Consumes: `_resolve_hermes_home()` (existing), filesystem at `~/.hermes/skills/`
- Produces: `{"skills": [...]}` dict

- [ ] **Step 1: Add _rpc_skills_list method**

```python
async def _rpc_skills_list(self) -> dict:
    hermes_home = self._resolve_hermes_home()
    skills_dir = os.path.join(hermes_home, "skills")
    if not os.path.isdir(skills_dir):
        return {"skills": []}

    skills = []
    for entry in sorted(os.listdir(skills_dir)):
        skill_path = os.path.join(skills_dir, entry)
        # Could be a directory with SKILL.md or a standalone .md file
        skill_md = None
        if os.path.isdir(skill_path):
            skill_md = os.path.join(skill_path, "SKILL.md")
        elif entry.endswith(".md"):
            skill_md = skill_path

        if not skill_md or not os.path.isfile(skill_md):
            continue

        name = entry.replace(".md", "")
        description = ""
        category = ""
        try:
            with open(skill_md) as f:
                content = f.read()
            # Parse frontmatter
            if content.startswith("---"):
                parts = content.split("---", 2)
                if len(parts) >= 3:
                    import yaml
                    fm = yaml.safe_load(parts[1]) or {}
                    name = fm.get("name", name)
                    description = fm.get("description", "")
        except Exception:
            pass

        skills.append({
            "name": name,
            "description": description,
            "category": category,
            "path": skill_path,
        })

    return {"skills": skills}
```

- [ ] **Step 2: Add dispatch entry**

```python
elif method == "skills.list":
    result = await self._rpc_skills_list()
```

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "feat(connector): add skills.list RPC handler"
```

---

## Task 4: Connector — Add cron.list/create/update/delete RPC Handlers

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py` — add 4 cron RPC handlers + dispatch

**Interfaces:**
- Consumes: Hermes CLI (`hermes cron ...`) or Hermes API (`/v1/cron`)
- Produces: `{"jobs": [...]}` for list, `{"job": {...}}` for create/update, `{"deleted": true}` for delete

- [ ] **Step 1: Add _rpc_cron_list method**

```python
async def _rpc_cron_list(self) -> dict:
    """List scheduled cron jobs from Hermes."""
    import subprocess
    try:
        result = subprocess.run(
            ["hermes", "cron", "list", "--json"],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode == 0:
            import json
            return {"jobs": json.loads(result.stdout)}
    except Exception:
        pass
    return {"jobs": []}
```

- [ ] **Step 2: Add _rpc_cron_create method**

```python
async def _rpc_cron_create(self, params: dict) -> dict:
    """Create a new cron job."""
    import subprocess
    name = params.get("name", "")
    schedule = params.get("schedule", "")
    prompt = params.get("prompt", "")
    try:
        result = subprocess.run(
            ["hermes", "cron", "add", "--name", name, "--schedule", schedule, "--prompt", prompt, "--json"],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode == 0:
            import json
            return {"job": json.loads(result.stdout)}
    except Exception:
        pass
    raise Exception("Failed to create cron job")
```

- [ ] **Step 3: Add _rpc_cron_update method**

```python
async def _rpc_cron_update(self, params: dict) -> dict:
    """Update an existing cron job."""
    import subprocess
    job_id = params.get("id", "")
    args = ["hermes", "cron", "update", job_id, "--json"]
    if params.get("name"):
        args.extend(["--name", params["name"]])
    if params.get("schedule"):
        args.extend(["--schedule", params["schedule"]])
    if params.get("prompt"):
        args.extend(["--prompt", params["prompt"]])
    if params.get("enabled") is not None:
        args.extend(["--enabled", str(params["enabled"]).lower()])
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=10)
        if result.returncode == 0:
            import json
            return {"job": json.loads(result.stdout)}
    except Exception:
        pass
    raise Exception("Failed to update cron job")
```

- [ ] **Step 4: Add _rpc_cron_delete method**

```python
async def _rpc_cron_delete(self, params: dict) -> dict:
    """Delete a cron job."""
    import subprocess
    job_id = params.get("id", "")
    try:
        result = subprocess.run(
            ["hermes", "cron", "remove", job_id],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode == 0:
            return {"deleted": True}
    except Exception:
        pass
    raise Exception("Failed to delete cron job")
```

- [ ] **Step 5: Add dispatch entries**

```python
elif method == "cron.list":
    result = await self._rpc_cron_list()
elif method == "cron.create":
    result = await self._rpc_cron_create(params)
elif method == "cron.update":
    result = await self._rpc_cron_update(params)
elif method == "cron.delete":
    result = await self._rpc_cron_delete(params)
```

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "feat(connector): add cron.list/create/update/delete RPC handlers"
```

---

## Task 5: Connector — Add memories.list and tools.list RPC Handlers

**Files:**
- Modify: `connector/src/hermes_mobile_connector/client.py` — add 2 handlers + dispatch

- [ ] **Step 1: Add _rpc_memories_list method**

```python
async def _rpc_memories_list(self) -> dict:
    hermes_home = self._resolve_hermes_home()
    memory_file = os.path.join(hermes_home, "memories", "MEMORY.md")
    if not os.path.isfile(memory_file):
        return {"memories": []}
    memories = []
    try:
        with open(memory_file) as f:
            for line in f:
                line = line.strip()
                if line.startswith("- ["):
                    # Format: - [Title](file.md) — description
                    parts = line.split("]", 1)
                    if len(parts) >= 2:
                        title = parts[0].replace("- [", "")
                        rest = parts[1]
                        desc = rest.split("—", 1)[-1].strip() if "—" in rest else ""
                        memories.append({"name": title, "description": desc})
    except Exception:
        pass
    return {"memories": memories}
```

- [ ] **Step 2: Add _rpc_tools_list method**

```python
async def _rpc_tools_list(self) -> dict:
    """List available MCP tools from Hermes config."""
    hermes_home = self._resolve_hermes_home()
    config_path = os.path.join(hermes_home, "config.yaml")
    if not os.path.isfile(config_path):
        return {"tools": []}
    try:
        import yaml
        with open(config_path) as f:
            config = yaml.safe_load(f) or {}
        mcp_servers = config.get("mcp_servers", [])
        tools = []
        for server in mcp_servers:
            if isinstance(server, dict):
                tools.append({
                    "name": server.get("name", "unknown"),
                    "command": server.get("command", ""),
                })
        return {"tools": tools}
    except Exception:
        return {"tools": []}
```

- [ ] **Step 3: Add dispatch entries**

```python
elif method == "memories.list":
    result = await self._rpc_memories_list()
elif method == "tools.list":
    result = await self._rpc_tools_list()
```

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add connector/src/hermes_mobile_connector/client.py
git commit -m "feat(connector): add memories.list and tools.list RPC handlers"
```

---

## Task 6: iOS — ProfileStore

**Files:**
- Create: `HermesMobile/Stores/ProfileStore.swift`

**Interfaces:**
- Consumes: `RelayAPIClient.get(path:accessToken:)` (existing)
- Produces: `ProfileStore` consumed by `ProfileSelectorSheet` and `ChatScreen`

- [ ] **Step 1: Create ProfileStore.swift**

```swift
import Foundation

@MainActor
@Observable
final class ProfileStore {
    struct HermesProfile: Codable, Identifiable, Hashable {
        var id: String { name }
        let name: String
        let description: String
        let skillCount: Int
    }

    struct ProfileCatalogResponse: Decodable {
        let activeProfile: HermesProfile?
        let profiles: [HermesProfile]
    }

    private(set) var profiles: [HermesProfile] = []
    private(set) var activeProfileName: String?
    private(set) var isLoading = false
    private(set) var lastError: String?

    private var lastFetchedAt: Date?
    private let cacheInterval: TimeInterval = 60

    private let relayClient: RelayAPIClient
    private let accessTokenProvider: () -> String?

    init(relayClient: RelayAPIClient, accessTokenProvider: @escaping () -> String?) {
        self.relayClient = relayClient
        self.accessTokenProvider = accessTokenProvider
    }

    var activeProfile: HermesProfile? {
        guard let name = activeProfileName else { return nil }
        return profiles.first { $0.name == name }
    }

    func loadProfiles(forceRefresh: Bool = false) async {
        if !forceRefresh,
           let lastFetchedAt,
           Date().timeIntervalSince(lastFetchedAt) < cacheInterval {
            return
        }
        guard let token = accessTokenProvider() else { return }
        isLoading = true
        lastError = nil
        do {
            let response: ProfileCatalogResponse = try await relayClient.get(
                path: "profiles", accessToken: token
            )
            profiles = response.profiles
            activeProfileName = response.activeProfile?.name
            lastFetchedAt = Date()
        } catch {
            lastError = error.localizedDescription
        }
        isLoading = false
    }

    func markActive(_ name: String) {
        activeProfileName = name
    }
}
```

- [ ] **Step 2: Wire ProfileStore into AppContainer**

In `HermesMobile/Stores/AppContainer.swift`, add `profileStore` property and initialize it alongside `modelStore`.

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Stores/ProfileStore.swift HermesMobile/Stores/AppContainer.swift
git commit -m "feat(ios): add ProfileStore for profile catalog"
```

---

## Task 7: iOS — ProfileSelectorSheet

**Files:**
- Create: `HermesMobile/Features/Chat/ProfileSelectorSheet.swift`

**Interfaces:**
- Consumes: `ProfileStore` (Task 6)
- Produces: SwiftUI sheet presented from `ChatScreen` toolbar

- [ ] **Step 1: Create ProfileSelectorSheet.swift**

```swift
import SwiftUI

struct ProfileSelectorSheet: View {
    let profiles: [ProfileStore.HermesProfile]
    let activeProfileName: String?
    let onSelect: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                if profiles.isEmpty {
                    ContentUnavailableView(
                        "No Profiles",
                        systemImage: "brain.head.profile",
                        description: Text("No Hermes profiles are available.")
                    )
                } else {
                    ForEach(profiles) { profile in
                        Button {
                            onSelect(profile.name)
                            dismiss()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(profile.name)
                                        .font(.headline)
                                    if !profile.description.isEmpty {
                                        Text(profile.description)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(2)
                                    }
                                    Text("\(profile.skillCount) skills")
                                        .font(.caption2)
                                        .foregroundStyle(.tertiary)
                                }
                                Spacer()
                                if profile.name == activeProfileName {
                                    Image(systemName: "checkmark.circle.fill")
                                        .foregroundStyle(.green)
                                }
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
            .navigationTitle("Profile")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }
}
```

- [ ] **Step 2: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ProfileSelectorSheet.swift
git commit -m "feat(ios): add ProfileSelectorSheet"
```

---

## Task 8: iOS — Profile Chip in Chat Toolbar

**Files:**
- Modify: `HermesMobile/Features/Chat/ChatScreen.swift` — add profile chip + sheet presentation

**Interfaces:**
- Consumes: `ProfileStore`, `ProfileSelectorSheet`, `ChatStore`
- Produces: Profile chip in toolbar, `/profile <name>` message dispatch

- [ ] **Step 1: Add profileStore to ChatScreen**

In `ChatScreen.swift`, add `@Environment(ProfileStore.self) private var profileStore` and a `@State private var showProfileSelector = false`.

- [ ] **Step 2: Add profile chip to toolbar**

In the toolbar section, add a profile chip before the model chip:

```swift
if profileStore.activeProfile != nil {
    Button {
        showProfileSelector = true
    } label: {
        HStack(spacing: 4) {
            Image(systemName: "brain.head.profile")
            Text(profileStore.activeProfileName ?? "Profile")
                .font(.caption)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.ultraThinMaterial, in: Capsule())
    }
    .popover(isPresented: $showProfileSelector) {
        ProfileSelectorSheet(
            profiles: profileStore.profiles,
            activeProfileName: profileStore.activeProfileName
        ) { name in
            profileStore.markActive(name)
            chatStore.sendMessage("/profile \(name)")
        }
        .presentationDetents([.medium])
    }
}
```

- [ ] **Step 3: Add profile switch detection to ChatStore**

In `ChatStore.swift`, add a method to detect profile switch confirmations in assistant responses:

```swift
func detectProfileSwitch(in text: String) {
    let pattern = #"(?:switched|changed|activated).*?profile.*?(?:to|:)\s*["']?(\w+)["']?"#
    if let regex = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive),
       let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
       let range = Range(match.range(at: 1), in: text) {
        let profileName = String(text[range])
        profileStore?.markActive(profileName)
    }
}
```

- [ ] **Step 4: Load profiles on chat screen appear**

Add `.task { await profileStore.loadProfiles() }` to ChatScreen's body.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ChatScreen.swift HermesMobile/Stores/ChatStore.swift
git commit -m "feat(ios): add profile chip to chat toolbar with switch support"
```

---

## Task 9: iOS — Session History Polish

**Files:**
- Modify: `HermesMobile/Features/Sidebar/` — sidebar session list views
- Modify: `HermesMobile/Stores/SessionListStore.swift` — add filter state

**Interfaces:**
- Consumes: `SessionListStore` (existing), `SessionSummary` (existing)
- Produces: Polished sidebar with filters, date headers, preview text, swipe actions

- [ ] **Step 1: Add filter enum and state to SessionListStore**

```swift
enum SessionFilter: String, CaseIterable {
    case all = "All"
    case pinned = "Pinned"
    case archived = "Archived"
}

// Add to SessionListStore:
var activeFilter: SessionFilter = .all
var filteredSessions: [SessionSummary] {
    switch activeFilter {
    case .all:
        return sessions.filter { !$0.isArchived }
    case .pinned:
        return sessions.filter { $0.isPinned && !$0.isArchived }
    case .archived:
        return sessions.filter { $0.isArchived }
    }
}
```

- [ ] **Step 2: Add date section grouping**

```swift
struct SessionSection: Identifiable {
    let id: String
    let title: String
    let sessions: [SessionSummary]
}

var sessionSections: [SessionSection] {
    let calendar = Calendar.current
    let grouped = Dictionary(grouping: filteredSessions) { session -> String in
        if calendar.isDateInToday(session.lastActivity) { return "Today" }
        if calendar.isDateInYesterday(session.lastActivity) { return "Yesterday" }
        if calendar.isDate(session.lastActivity, equalTo: Date(), toGranularity: .weekOfYear) { return "This Week" }
        return "Older"
    }
    let order = ["Today", "Yesterday", "This Week", "Older"]
    return order.compactMap { key in
        guard let sessions = grouped[key], !sessions.isEmpty else { return nil }
        return SessionSection(id: key, title: key, sessions: sessions)
    }
}
```

- [ ] **Step 3: Update sidebar view with filter chips and date sections**

Add a `Picker` or `HStack` of filter chips above the session list. Use `sessionSections` to render `Section` headers. Show `previewText` as a subtitle under each session title.

- [ ] **Step 4: Add swipe actions**

```swift
.swipeActions(edge: .leading) {
    Button {
        sessionStore.togglePin(session)
    } label: {
        Label(session.isPinned ? "Unpin" : "Pin", systemImage: session.isPinned ? "pin.slash" : "pin")
    }
    .tint(.yellow)
}
.swipeActions(edge: .trailing) {
    Button(role: .destructive) {
        sessionStore.archiveSession(session)
    } label: {
        Label("Archive", systemImage: "archivebox")
    }
    Button(role: .destructive) {
        sessionStore.deleteSession(session)
    } label: {
        Label("Delete", systemImage: "trash")
    }
}
```

- [ ] **Step 5: Add empty states**

```swift
if filteredSessions.isEmpty {
    ContentUnavailableView(
        activeFilter == .archived ? "No Archived Sessions" : "No Sessions Yet",
        systemImage: "bubble.left.and.bubble.right",
        description: Text(activeFilter == .archived ? "Archived sessions will appear here." : "Start a conversation to create your first session.")
    )
}
```

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Sidebar/ HermesMobile/Stores/SessionListStore.swift
git commit -m "feat(ios): polish session history with filters, date sections, swipe actions, empty states"
```

---

## Task 10: iOS — SkillsStore

**Files:**
- Create: `HermesMobile/Stores/SkillsStore.swift`

**Interfaces:**
- Consumes: `RelayAPIClient.get(path:accessToken:)` (existing)
- Produces: `SkillsStore` consumed by `SkillsBrowserView`

- [ ] **Step 1: Create SkillsStore.swift**

```swift
import Foundation

@MainActor
@Observable
final class SkillsStore {
    struct HermesSkill: Codable, Identifiable, Hashable {
        var id: String { name }
        let name: String
        let description: String
        let category: String
        let path: String
    }

    struct SkillCatalogResponse: Decodable {
        let skills: [HermesSkill]
    }

    private(set) var skills: [HermesSkill] = []
    private(set) var isLoading = false
    private(set) var lastError: String?
    var searchText = ""

    private var lastFetchedAt: Date?
    private let cacheInterval: TimeInterval = 120

    private let relayClient: RelayAPIClient
    private let accessTokenProvider: () -> String?

    init(relayClient: RelayAPIClient, accessTokenProvider: @escaping () -> String?) {
        self.relayClient = relayClient
        self.accessTokenProvider = accessTokenProvider
    }

    var filteredSkills: [HermesSkill] {
        if searchText.isEmpty { return skills }
        return skills.filter {
            $0.name.localizedCaseInsensitiveContains(searchText) ||
            $0.description.localizedCaseInsensitiveContains(searchText)
        }
    }

    var skillsByCategory: [String: [HermesSkill]] {
        Dictionary(grouping: filteredSkills, by: { $0.category.isEmpty ? "General" : $0.category })
    }

    func loadSkills(forceRefresh: Bool = false) async {
        if !forceRefresh,
           let lastFetchedAt,
           Date().timeIntervalSince(lastFetchedAt) < cacheInterval {
            return
        }
        guard let token = accessTokenProvider() else { return }
        isLoading = true
        lastError = nil
        do {
            let response: SkillCatalogResponse = try await relayClient.get(
                path: "skills", accessToken: token
            )
            skills = response.skills
            lastFetchedAt = Date()
        } catch {
            lastError = error.localizedDescription
        }
        isLoading = false
    }
}
```

- [ ] **Step 2: Wire SkillsStore into AppContainer**

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Stores/SkillsStore.swift HermesMobile/Stores/AppContainer.swift
git commit -m "feat(ios): add SkillsStore for skill catalog"
```

---

## Task 11: iOS — Skills Browser Screen

**Files:**
- Create: `HermesMobile/Features/Skills/SkillsBrowserView.swift`
- Create: `HermesMobile/Features/Skills/SkillDetailView.swift`
- Modify: `HermesMobile/Features/Sidebar/` — add Skills sidebar entry

**Interfaces:**
- Consumes: `SkillsStore` (Task 10)
- Produces: New sidebar entry + NavigationStack screen

- [ ] **Step 1: Create SkillDetailView.swift**

```swift
import SwiftUI

struct SkillDetailView: View {
    let skill: SkillsStore.HermesSkill

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if !skill.description.isEmpty {
                    Text(skill.description)
                        .font(.body)
                }
                if !skill.category.isEmpty {
                    Label(skill.category, systemImage: "folder")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Divider()
                Label(skill.path, systemImage: "doc.text")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
            }
            .padding()
        }
        .navigationTitle(skill.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}
```

- [ ] **Step 2: Create SkillsBrowserView.swift**

```swift
import SwiftUI

struct SkillsBrowserView: View {
    @Environment(SkillsStore.self) private var skillsStore

    var body: some View {
        List {
            if skillsStore.isLoading && skillsStore.skills.isEmpty {
                ProgressView("Loading skills...")
            } else if skillsStore.filteredSkills.isEmpty {
                ContentUnavailableView(
                    "No Skills",
                    systemImage: "wrench.and.screwdriver",
                    description: Text(skillsStore.searchText.isEmpty
                        ? "No skills are installed."
                        : "No skills match '\(skillsStore.searchText)'.")
                )
            } else {
                ForEach(Array(skillsStore.skillsByCategory.keys.sorted()), id: \.self) { category in
                    Section(category) {
                        ForEach(skillsStore.skillsByCategory[category] ?? []) { skill in
                            NavigationLink(value: skill) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(skill.name)
                                        .font(.headline)
                                    if !skill.description.isEmpty {
                                        Text(skill.description)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(2)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        .searchable(text: Bindable(skillsStore).searchText, prompt: "Search skills")
        .navigationTitle("Skills")
        .navigationDestination(for: SkillsStore.HermesSkill.self) { skill in
            SkillDetailView(skill: skill)
        }
        .refreshable { await skillsStore.loadSkills(forceRefresh: true) }
        .task { await skillsStore.loadSkills() }
    }
}
```

- [ ] **Step 3: Add sidebar entry**

In the sidebar view, add a NavigationLink for Skills:

```swift
NavigationLink {
    SkillsBrowserView()
} label: {
    Label("Skills", systemImage: "wrench.and.screwdriver")
}
```

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Skills/ HermesMobile/Features/Sidebar/
git commit -m "feat(ios): add Skills Browser screen with search and detail view"
```

---

## Task 12: iOS — CronStore

**Files:**
- Create: `HermesMobile/Stores/CronStore.swift`

**Interfaces:**
- Consumes: `RelayAPIClient` (existing)
- Produces: `CronStore` consumed by `CronManagerView`

- [ ] **Step 1: Create CronStore.swift**

```swift
import Foundation

@MainActor
@Observable
final class CronStore {
    struct CronJob: Codable, Identifiable, Hashable {
        let id: String
        let name: String
        let schedule: String
        let prompt: String
        var enabled: Bool
        let lastRun: Date?
        let nextRun: Date?
        let lastResult: String?
    }

    struct CronListResponse: Decodable {
        let jobs: [CronJob]
    }

    struct CronJobResponse: Decodable {
        let job: CronJob
    }

    private(set) var jobs: [CronJob] = []
    private(set) var isLoading = false
    private(set) var lastError: String?

    private let relayClient: RelayAPIClient
    private let accessTokenProvider: () -> String?

    init(relayClient: RelayAPIClient, accessTokenProvider: @escaping () -> String?) {
        self.relayClient = relayClient
        self.accessTokenProvider = accessTokenProvider
    }

    func loadJobs() async {
        guard let token = accessTokenProvider() else { return }
        isLoading = true
        lastError = nil
        do {
            let response: CronListResponse = try await relayClient.get(
                path: "cron", accessToken: token
            )
            jobs = response.jobs
        } catch {
            lastError = error.localizedDescription
        }
        isLoading = false
    }

    func createJob(name: String, schedule: String, prompt: String) async throws {
        guard let token = accessTokenProvider() else { return }
        let body = ["name": name, "schedule": schedule, "prompt": prompt]
        let response: CronJobResponse = try await relayClient.post(
            path: "cron", body: body, accessToken: token
        )
        jobs.append(response.job)
    }

    func toggleJob(_ job: CronJob) async throws {
        guard let token = accessTokenProvider() else { return }
        let body = ["enabled": !job.enabled]
        let response: CronJobResponse = try await relayClient.patch(
            path: "cron/\(job.id)", body: body, accessToken: token
        )
        if let index = jobs.firstIndex(where: { $0.id == job.id }) {
            jobs[index] = response.job
        }
    }

    func deleteJob(_ job: CronJob) async throws {
        guard let token = accessTokenProvider() else { return }
        let _: EmptyResponse = try await relayClient.delete(
            path: "cron/\(job.id)", accessToken: token
        )
        jobs.removeAll { $0.id == job.id }
    }
}
```

- [ ] **Step 2: Wire CronStore into AppContainer**

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Stores/CronStore.swift HermesMobile/Stores/AppContainer.swift
git commit -m "feat(ios): add CronStore for cron job management"
```

---

## Task 13: iOS — Cron Manager Screen

**Files:**
- Create: `HermesMobile/Features/Cron/CronManagerView.swift`
- Create: `HermesMobile/Features/Cron/CronJobDetailView.swift`
- Create: `HermesMobile/Features/Cron/CreateCronJobSheet.swift`
- Modify: `HermesMobile/Features/Sidebar/` — add Cron sidebar entry

**Interfaces:**
- Consumes: `CronStore` (Task 12)
- Produces: New sidebar entry + NavigationStack screen

- [ ] **Step 1: Create CronJobDetailView.swift**

```swift
import SwiftUI

struct CronJobDetailView: View {
    let job: CronStore.CronJob

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                LabeledContent("Schedule", value: job.schedule)
                if let lastRun = job.lastRun {
                    LabeledContent("Last Run", value: lastRun.formatted())
                }
                if let nextRun = job.nextRun {
                    LabeledContent("Next Run", value: nextRun.formatted())
                }
                if let result = job.lastResult, !result.isEmpty {
                    Divider()
                    Text("Last Result")
                        .font(.headline)
                    Text(result)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Divider()
                Text("Prompt")
                    .font(.headline)
                Text(job.prompt)
                    .font(.body)
            }
            .padding()
        }
        .navigationTitle(job.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}
```

- [ ] **Step 2: Create CreateCronJobSheet.swift**

```swift
import SwiftUI

struct CreateCronJobSheet: View {
    let onCreate: (String, String, String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var schedule = ""
    @State private var prompt = ""

    private let presets = [
        ("Every hour", "0 * * * *"),
        ("Daily at 9am", "0 9 * * *"),
        ("Weekdays at 9am", "0 9 * * 1-5"),
        ("Weekly (Monday 9am)", "0 9 * * 1"),
    ]

    var body: some View {
        NavigationStack {
            Form {
                Section("Details") {
                    TextField("Name", text: $name)
                    TextField("Cron expression", text: $schedule)
                        .font(.caption)
                }
                Section("Quick Presets") {
                    ForEach(presets, id: \.1) { preset in
                        Button {
                            schedule = preset.1
                        } label: {
                            HStack {
                                Text(preset.0)
                                Spacer()
                                Text(preset.1)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                Section("Prompt") {
                    TextEditor(text: $prompt)
                        .frame(minHeight: 100)
                }
            }
            .navigationTitle("New Cron Job")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        onCreate(name, schedule, prompt)
                        dismiss()
                    }
                    .disabled(name.isEmpty || schedule.isEmpty || prompt.isEmpty)
                }
            }
        }
    }
}
```

- [ ] **Step 3: Create CronManagerView.swift**

```swift
import SwiftUI

struct CronManagerView: View {
    @Environment(CronStore.self) private var cronStore
    @State private var showCreateSheet = false

    var body: some View {
        List {
            if cronStore.isLoading && cronStore.jobs.isEmpty {
                ProgressView("Loading cron jobs...")
            } else if cronStore.jobs.isEmpty {
                ContentUnavailableView(
                    "No Cron Jobs",
                    systemImage: "clock.badge",
                    description: Text("Create a scheduled job to automate tasks.")
                )
            } else {
                ForEach(cronStore.jobs) { job in
                    NavigationLink(value: job) {
                        HStack {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(job.name)
                                    .font(.headline)
                                Text(job.schedule)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                if let nextRun = job.nextRun {
                                    Text("Next: \(nextRun.formatted(.relative(presentation: .named)))")
                                        .font(.caption2)
                                        .foregroundStyle(.tertiary)
                                }
                            }
                            Spacer()
                            Toggle("", isOn: Binding(
                                get: { job.enabled },
                                set: { _ in
                                    Task { try? await cronStore.toggleJob(job) }
                                }
                            ))
                            .labelsHidden()
                        }
                    }
                    .swipeActions {
                        Button(role: .destructive) {
                            Task { try? await cronStore.deleteJob(job) }
                        } label: {
                            Label("Delete", systemImage: "trash")
                        }
                    }
                }
            }
        }
        .navigationTitle("Cron Jobs")
        .navigationDestination(for: CronStore.CronJob.self) { job in
            CronJobDetailView(job: job)
        }
        .refreshable { await cronStore.loadJobs() }
        .task { await cronStore.loadJobs() }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    showCreateSheet = true
                } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .sheet(isPresented: $showCreateSheet) {
            CreateCronJobSheet { name, schedule, prompt in
                Task { try? await cronStore.createJob(name: name, schedule: schedule, prompt: prompt) }
            }
        }
    }
}
```

- [ ] **Step 4: Add sidebar entry**

```swift
NavigationLink {
    CronManagerView()
} label: {
    Label("Cron Jobs", systemImage: "clock.badge")
}
```

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Cron/ HermesMobile/Features/Sidebar/
git commit -m "feat(ios): add Cron Manager screen with create, toggle, delete"
```

---

## Task 14: End-to-End Verification

- [ ] **Step 1: Deploy relay changes**

Push relay changes to the relay host and restart the relay service. Verify all 6 new endpoints respond (even with empty defaults when connector is offline).

```bash
curl -s https://your-relay.example.com/v1/profiles -H "Authorization: Bearer <token>" | jq .
curl -s https://your-relay.example.com/v1/skills -H "Authorization: Bearer <token>" | jq .
curl -s https://your-relay.example.com/v1/cron -H "Authorization: Bearer <token>" | jq .
curl -s https://your-relay.example.com/v1/memories -H "Authorization: Bearer <token>" | jq .
curl -s https://your-relay.example.com/v1/tools -H "Authorization: Bearer <token>" | jq .
```

- [ ] **Step 2: Deploy connector changes**

Update connector on ignyte host, restart `hermes-mobile-connector.service`. Verify RPC handlers respond:

```bash
# Check connector logs for RPC handler registration
journalctl --user -u hermes-mobile-connector -f
```

- [ ] **Step 3: Build and install iOS app**

Build on MBP, install on iPhone/iPad. Verify:
- Profile chip appears in chat toolbar, shows active profile
- Skills sidebar entry works, shows installed skills
- Cron Jobs sidebar entry works (may be empty if no jobs exist)
- Session history shows filter chips, date sections, preview text

- [ ] **Step 4: Final commit**

```bash
cd ~/Hermes-iOS
git commit --allow-empty -m "chore: verify end-to-end profile, skills, cron, session polish"
```
