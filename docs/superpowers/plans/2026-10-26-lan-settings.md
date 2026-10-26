# LAN Settings Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a gear button that opens a settings page with global LAN on/off toggles for grit, folio and krust, each writing that app's config file.

**Architecture:** Each of the three apps gets a uniform `host` string in its own `config.json` (`127.0.0.1` off, `0.0.0.0` on). Grit is the writer for all three. Grit's own toggle is applied live by swapping its TCP listener in place; krust and folio take effect on their next process start, with an explicit "restart" button that stops and respawns them. A small HTTP surface (`GET/POST /settings`, `POST /settings/restart`) is separate from the WebSocket git protocol.

**Tech Stack:** Rust (tokio, axum 0.8, serde_json, tower-http), vanilla ES modules in `web/dist/` embedded via rust-embed, plus the sibling repos `~/Projects/krust` and `~/Projects/folio`.

**Spec:** `docs/superpowers/specs/2026-10-26-lan-settings-and-sync-badge-design.md` (Part 1)

## Global Constraints

- Single self-contained binary; no runtime dependency on loose external files (AGENTS.md §3.4).
- `web/dist/*` is embedded at compile time: web changes need a rebuild and a restart to take effect (AGENTS.md §4).
- JS syntax gate for every edited module: `cp web/dist/<f>.js /tmp/x.mjs && node --check /tmp/x.mjs`.
- Never block a runtime worker on git or filesystem I/O; use `tokio::task::spawn_blocking` (AGENTS.md §3.2).
- Do not kill or restart running daemons except through the explicit restart button (AGENTS.md §4).
- `/proc` use is Linux-only and consistent with `src/actions.rs`; no new platform abstraction.
- The only new dependency is `libc` (unix, for `kill(2)`).
- `cargo check` and `cargo test` must pass after every task in this repo; `cargo test` in krust/folio likewise.

## Review Focus

1. A grit `config.json` written before `host` existed must keep its tabs (Task 1).
2. An absent, empty or malformed krust/folio config must never be clobbered or left invalid (Task 2).
3. A bind failure or an unparseable host string on toggle must leave grit reachable (Task 3).
4. A process that is not krust/folio must never be killed by the restart (Task 6).
5. A LAN browser must be accepted by the origin check and the docks must point at the server, not the client (Tasks 4, 9).

---

### Task 1: `host` in grit's own config

**Files:**
- Modify: `src/shared_config.rs`
- Test: `src/shared_config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `GritConfig.host: String`; `pub fn default_host() -> String`; `pub fn load_config_from(path: &Path) -> GritConfig`; `pub fn save_config_to(path: &Path, cfg: &GritConfig)`; `impl Default for GritConfig`; `pub fn config_write_lock() -> &'static std::sync::Mutex<()>`.

**Why the lock:** `persist_web_state` (`shared_config.rs:101`) read-modify-writes the same file on every registry change — that is, whenever a tab opens or closes. The settings endpoint writing it too is a lost-update race. One process-wide lock, taken around the read *and* the write, closes it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn config_without_host_keeps_tabs_and_defaults_to_loopback() {
    // The whole feature hangs on this: a file written by an older grit has
    // no `host`, and `load_config` returns EMPTY TABS on a parse failure.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, br#"{"tabs":[{"id":1,"name":"t","path":"/p"}]}"#).unwrap();
    let cfg = load_config_from(&path);
    assert_eq!(cfg.host, "127.0.0.1");
    assert_eq!(cfg.tabs.len(), 1);
}

#[test]
fn host_round_trips_through_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = GritConfig { tabs: vec![], host: "0.0.0.0".to_string() };
    save_config_to(&path, &cfg);
    assert_eq!(load_config_from(&path).host, "0.0.0.0");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib shared_config::`
Expected: FAIL — `load_config_from` and the `host` field do not exist.

- [ ] **Step 3: Implement**

Add to `GritConfig` (the `#[serde(default = "default_host")]` is mandatory — without it every existing config loses its tabs) and add the two `_from`/`_to` helpers, having `load_config`/`save_config` delegate to them. Add the `config_write_lock()` `static Mutex<()>` and take it inside `save_config` and inside `persist_web_state`'s read-modify-write.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib shared_config::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/shared_config.rs
git commit -m "feat(config): add host to grit config, defaulting to loopback"
```

---

### Task 2: settings module — paths and config read-modify-write

**Files:**
- Create: `src/settings.rs`
- Modify: `src/main.rs` (add `mod settings;`)
- Test: `src/settings.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const LOOPBACK: &str = "127.0.0.1";` `pub const ANY: &str = "0.0.0.0";`
  - `pub fn is_lan(host: &str) -> bool` — true unless `host` is a loopback literal.
  - `pub enum Service { Grit, Folio, Krust }` with `pub fn key(self) -> &'static str` (`"grit"`/`"folio"`/`"krust"`), `pub fn label(self) -> &'static str`, `pub const ALL: [Service; 3]`.
  - `pub struct SettingsPaths { pub grit: Option<PathBuf>, pub krust: Option<PathBuf>, pub folio: Option<PathBuf> }`
  - `pub fn resolve_paths(xdg: Option<PathBuf>, home: Option<PathBuf>) -> SettingsPaths` (pure) and `impl SettingsPaths { pub fn from_env() -> Self }`.
  - `pub fn read_host(path: &Path) -> String`
  - `pub fn write_host(path: &Path, host: &str) -> std::io::Result<()>`
  - `pub fn read_port(path: &Path, default: u16) -> u16`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn paths_mirror_each_apps_own_rule() {
    let paths = resolve_paths(Some(PathBuf::from("/xdg")), Some(PathBuf::from("/home/u")));
    assert_eq!(paths.grit, Some(PathBuf::from("/xdg/bitshift/grit/config.json")));
    assert_eq!(paths.krust, Some(PathBuf::from("/xdg/bitshift/krust/config.json")));
    // No XDG: every app falls back to $HOME/.config with the same tail.
    let paths = resolve_paths(None, Some(PathBuf::from("/home/u")));
    assert_eq!(paths.folio, Some(PathBuf::from("/home/u/.config/bitshift/folio/config.json")));
}

#[test]
fn write_host_preserves_fields_grit_does_not_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, br#"{"host":"127.0.0.1","port":3000,"allowed_origins":["http://localhost:3000"]}"#).unwrap();
    write_host(&path, ANY).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["host"], "0.0.0.0");
    assert_eq!(value["port"], 3000);
    assert_eq!(value["allowed_origins"][0], "http://localhost:3000");
}

#[test]
fn write_host_creates_absent_file_and_parents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep/bitshift/folio/config.json");
    write_host(&path, ANY).unwrap();
    assert_eq!(read_host(&path), "0.0.0.0");
}

#[test]
fn write_host_refuses_to_destroy_a_file_it_cannot_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, b"not json").unwrap();
    assert!(write_host(&path, ANY).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not json");
}

#[test]
fn read_host_defaults_to_loopback_when_absent_or_invalid() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_host(&dir.path().join("missing.json")), LOOPBACK);
    let bad = dir.path().join("bad.json");
    fs::write(&bad, b"not json").unwrap();
    assert_eq!(read_host(&bad), LOOPBACK);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib settings::`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement `src/settings.rs`**

`write_host` reads the document as `serde_json::Value` (creating `{}` when the file is absent), inserts `host`, creates parent directories, and writes pretty JSON. A malformed document returns `Err(ErrorKind::InvalidData)` **without touching the file**. It takes `shared_config::config_write_lock()` around the read-modify-write so it cannot race grit's own persister. `read_port` reads the numeric `port` field. `resolve_paths` mirrors each app's rule: grit filters `XDG_CONFIG_HOME` to absolute paths, krust and folio do not.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib settings::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs src/main.rs
git commit -m "feat(settings): config path resolution and host read-modify-write"
```

---

### Task 3: live listener rebind

**Files:**
- Modify: `src/server/mod.rs` (`ServerControl`, `create_listener`, `serve_listener`, `run`, `boot`), `src/main.rs`
- Test: `src/server/mod.rs`, `src/server/websocket.rs:1320,1345` (call-site updates)

**Interfaces:**
- Consumes: `shared_config::load_config().host` (Task 1).
- Produces:
  - `pub struct ServerControl` with `pub fn new(initial: String) -> Arc<Self>`, `pub fn loopback() -> Arc<Self>`, `pub fn desired(&self) -> String`, `pub fn effective(&self) -> String`, `pub fn bound(&self) -> Option<std::net::SocketAddr>`, `pub fn last_error(&self) -> Option<String>`, `pub fn request_host(&self, host: String)`, `pub async fn wait_for_change(&self)`.
  - `pub(crate) fn parse_host(host: &str) -> std::io::Result<std::net::Ipv4Addr>`
  - `pub(crate) async fn create_listener(host: &str, port: u16) -> std::io::Result<tokio::net::TcpListener>`
  - `pub(crate) fn serve_listener(listener: tokio::net::TcpListener, app: AppState) -> tokio::task::JoinHandle<()>`
  - `pub async fn run(registry: TabRegistry, port: u16, control: Arc<ServerControl>)`
  - `pub async fn boot(registry: TabRegistry, control: Arc<ServerControl>) -> (AppState, mpsc::UnboundedReceiver<()>)`
  - `AppState.control: Arc<ServerControl>`

**Contract for `run` (this is the whole point of the task):**
- Binds `control.desired()`; on success records `effective`, the actual `bound` `SocketAddr`, and clears `last_error`.
- On a bind failure: records `last_error`, restores `effective` to the last successfully bound host, and re-binds that host, so the daemon is never left without a listener.
- `sync_loop` is spawned exactly once for the whole process, regardless of how many rebinds happen.
- An initial-bind failure keeps today's behaviour (log loudly on stderr, return, no daemon).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parse_host_accepts_the_three_forms_and_rejects_garbage() {
    assert_eq!(parse_host("127.0.0.1").unwrap().to_string(), "127.0.0.1");
    assert_eq!(parse_host("0.0.0.0").unwrap().to_string(), "0.0.0.0");
    assert_eq!(parse_host("localhost").unwrap().to_string(), "127.0.0.1");
    assert!(parse_host("not-an-ip").is_err());
}

#[tokio::test]
async fn an_established_connection_survives_a_host_change() {
    // The settings page is itself a live connection when the user flips grit's
    // own toggle; losing it would make the feature unusable.
    let control = ServerControl::new("127.0.0.1".to_string());
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let registry = TabRegistry::with_single_tab(0, "repo".to_string(), dir.path().to_path_buf());
    tokio::spawn(run(registry, 0, control.clone()));
    let bound = wait_for_bound(&control).await;

    let mut ws = connect_with_retry(&format!("ws://{bound}/ws")).await;
    control.request_host("0.0.0.0".to_string());
    // The socket must keep delivering state after the switch.
    let state = recv_state(&mut ws).await;
    assert!(state.tabs.len() == 1);
}
```

Also add `failed_host_change_keeps_the_daemon_reachable`: request `"not-an-ip"`, assert `control.last_error().is_some()`, `control.effective() == "127.0.0.1"`, and a fresh connection to the old bound address still succeeds.

Add a small `async fn wait_for_bound(control: &ServerControl) -> std::net::SocketAddr` test helper that polls `control.bound()` until it is `Some`, with a timeout — the supervisor binds asynchronously and port 0 means the test cannot know the address up front.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib server::tests::an_established`
Expected: FAIL — `ServerControl`/`run` signature do not exist.

- [ ] **Step 3: Implement**

Split today's `run_server` into `serve_listener` (accept loop only, no `sync_loop`) plus a thin `run_server` that keeps its current signature and body for the existing tests. Implement the supervisor loop in `run` per the contract above. `AppState::with_watcher_resets` initialises `control: ServerControl::loopback()`; `boot` takes the real one and stores it. Update `main.rs` to load `shared_config::load_config().host` and pass `ServerControl::new(host)` at both `server::run` call sites (`src/main.rs:157`, `:192`). Update the two `create_listener` call sites in `src/server/websocket.rs` tests to `create_listener("127.0.0.1", port)`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS, whole suite (this task touches shared constructors).

- [ ] **Step 5: Commit**

```bash
git add src/server/mod.rs src/server/websocket.rs src/main.rs
git commit -m "feat(server): live listener rebind via ServerControl"
```

---

### Task 4: origin relaxation when LAN is on

**Files:**
- Modify: `src/server/websocket.rs`, `src/server/mod.rs` (CORS layer)
- Test: `src/server/websocket.rs`

**Interfaces:**
- Consumes: `ServerControl::effective()` (Task 3).
- Produces: `pub(crate) fn origin_is_allowed_for(origin: Option<&str>, lan: bool) -> bool`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn origin_is_allowed_for_accepts_any_when_lan_is_on() {
    assert!(origin_is_allowed_for(Some("http://192.168.1.9:5000"), true));
    assert!(origin_is_allowed_for(Some("http://evil.example"), true));
}

#[test]
fn origin_is_allowed_for_keeps_the_loopback_rule_when_lan_is_off() {
    assert!(!origin_is_allowed_for(Some("http://192.168.1.9:5000"), false));
    assert!(origin_is_allowed_for(Some("http://localhost:5000"), false));
    assert!(origin_is_allowed_for(None, false));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib origin_is_allowed_for`
Expected: FAIL — function does not exist.

- [ ] **Step 3: Implement**

`origin_is_allowed_for` returns `true` when `lan`, else delegates to the existing `origin_is_allowed`. The CORS predicate in `build_router` and `ws_handler` compute `lan` from `app.control.effective()` via `settings::is_lan`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib origin_is_allowed`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/websocket.rs src/server/mod.rs
git commit -m "feat(server): accept any origin when bound non-loopback"
```

---

### Task 5: `/settings` endpoints

**Files:**
- Modify: `src/settings.rs` (state + DTOs), `src/server/handlers.rs` (handlers), `src/server/mod.rs` (routes, `AppState` fields)
- Test: `src/server/handlers.rs`

**Interfaces:**
- Consumes: `SettingsPaths`, `read_host`, `write_host` (Task 2); `ServerControl` (Task 3).
- Produces:
  - `pub struct Baseline(std::sync::Mutex<std::collections::BTreeMap<Service, String>>)` with `pub fn capture(paths: &SettingsPaths) -> Self`, `pub fn get(&self, s: Service) -> String`, `pub fn recapture(&self, s: Service, host: String)`.
  - `#[derive(Serialize)] pub struct ServiceSettings { pub lan: bool, pub host: String, pub effective_host: Option<String>, pub error: Option<String>, pub pending_restart: Option<bool> }`
  - `#[derive(Serialize)] pub struct Ports { pub grit: u16, pub folio: u16, pub krust: u16 }`
  - `#[derive(Serialize)] pub struct SettingsResponse { pub grit: ServiceSettings, pub folio: ServiceSettings, pub krust: ServiceSettings, pub ports: Ports }`
  - `#[derive(Deserialize)] pub struct SettingsUpdate { pub grit: Option<bool>, pub folio: Option<bool>, pub krust: Option<bool> }`
  - `pub fn snapshot(paths: &SettingsPaths, control: &ServerControl, baseline: &Baseline, ports: Ports) -> SettingsResponse`
  - `pub fn apply(paths: &SettingsPaths, update: &SettingsUpdate, control: &ServerControl) -> Result<(), String>` — writes each present service's config and, for grit, calls `control.request_host`.
  - `AppState.settings_paths: SettingsPaths`, `AppState.baseline: Arc<Baseline>`, `pub fn with_settings(self, paths: SettingsPaths) -> Self`
  - Routes: `GET /settings` → `Json<SettingsResponse>`; `POST /settings` → `(StatusCode, Json<SettingsResponse>)`.

`pending_restart` appears only on folio/krust and is `Some(true)` when the saved host differs from `Baseline::get`. `effective_host` and `error` appear only on grit.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn settings_get_defaults_to_loopback_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let paths = SettingsPaths {
        grit: Some(dir.path().join("grit.json")),
        krust: Some(dir.path().join("krust.json")),
        folio: Some(dir.path().join("folio.json")),
    };
    let app = AppState::new(TabRegistry::new()).with_settings(paths);
    let body = oneshot_json(build_router(app), "/settings").await;
    assert_eq!(body["grit"]["lan"], false);
    assert_eq!(body["krust"]["host"], "127.0.0.1");
    assert_eq!(body["krust"]["pending_restart"], false);
}

#[tokio::test]
async fn settings_post_writes_the_config_and_reports_lan() {
    let dir = tempfile::tempdir().unwrap();
    let krust_path = dir.path().join("krust.json");
    fs::write(&krust_path, br#"{"port":3000,"allowed_origins":[]}"#).unwrap();
    let paths = SettingsPaths { grit: None, krust: Some(krust_path.clone()), folio: None };
    let app = AppState::new(TabRegistry::new()).with_settings(paths);

    let body = oneshot_post_json(build_router(app), "/settings", r#"{"krust":true}"#).await;
    assert_eq!(body["krust"]["lan"], true);
    assert_eq!(body["krust"]["pending_restart"], true);
    assert_eq!(read_host(&krust_path), "0.0.0.0");
    // Untouched fields survive the write.
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&krust_path).unwrap()).unwrap();
    assert_eq!(value["port"], 3000);
}

#[tokio::test]
async fn settings_post_on_grit_requests_a_live_rebind() {
    let app = AppState::new(TabRegistry::new()).with_settings(/* paths with grit: None */);
    let control = app.control.clone();
    oneshot_post_json(build_router(app), "/settings", r#"{"grit":true}"#).await;
    assert_eq!(control.desired(), "0.0.0.0");
}
```

Add the two test helpers (`oneshot_json`, `oneshot_post_json`) next to the existing oneshot helpers; they need `tower::ServiceExt`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib settings_get_defaults`
Expected: FAIL — route and DTOs do not exist.

- [ ] **Step 3: Implement**

Add the route to `build_router` (a literal path wins over the `/{*path}` static catch-all). Handlers wrap the file I/O in `spawn_blocking`. Capture the `Baseline` in `boot` from the `SettingsPaths` resolved by `from_env`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs src/server/handlers.rs src/server/mod.rs
git commit -m "feat(server): GET/POST /settings"
```

---

### Task 6: restart button

**Files:**
- Modify: `Cargo.toml` (add `libc`), `src/settings.rs`, `src/server/handlers.rs`, `src/server/mod.rs` (route)
- Test: `src/settings.rs`, `src/server/handlers.rs`

**Interfaces:**
- Consumes: `Service`, `SettingsPaths`, `Baseline` (Tasks 2, 5); `krust::ensure_krust`, `folio::ensure_folio` (existing).
- Produces:
  - `pub fn parse_listening_inodes(net_tcp: &str) -> Vec<(u16, u64)>` — pure parse of `/proc/net/tcp` LISTEN rows into `(port, inode)`.
  - `pub fn find_pid_for_port(net_tcp: &str, port: u16) -> Option<i32>` — scans `/proc/*/fd` for `socket:[inode]`.
  - `pub async fn restart_service(app: &AppState, service: Service) -> Result<(), String>` — SIGTERM (via `libc::kill`), poll the port free for up to 2s, SIGKILL, respawn through the existing `ensure_*`, then `Baseline::recapture`.
  - `POST /settings/restart` → `Json<serde_json::Value>` of the form `{"krust":{"ok":true,"error":null},"folio":{...}}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parse_listening_inodes_reads_only_listen_rows() {
    let fixture = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:C350 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0
   1: 0100007F:C351 00000000:0000 01 00000000:00000000 00:00000000 00000000  1000        0 99999 1 0000000000000000 100 0 0 10 0
";
    // 0xC350 == 50000, state 0A == LISTEN; the second row is ESTABLISHED (01).
    assert_eq!(parse_listening_inodes(fixture), vec![(50000, 12345)]);
}

#[test]
fn find_pid_for_port_finds_nothing_for_an_idle_port() {
    // Port 1 is never listened on by our tests; the scan must return None
    // rather than a random pid.
    assert!(find_pid_for_port(&read_proc_net_tcp(), 1).is_none());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib parse_listening_inodes`
Expected: FAIL — function does not exist.

- [ ] **Step 3: Implement**

Add `libc = "0.2"` under `[target.'cfg(unix)'.dependencies]`. `find_pid_for_port` matches each listening inode against `/proc/<pid>/fd/*` symlinks; an unmatched port returns `None`, which makes `restart_service` fall straight through to `ensure_*` (the "nothing listening → just spawn" path). Never signal a pid that was not found through the port.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/settings.rs src/server/handlers.rs src/server/mod.rs
git commit -m "feat(settings): restart krust and folio via /proc port lookup"
```

---

### Task 7: krust — config is the only source of truth

**Repo:** `~/Projects/krust`

**Files:**
- Modify: `server/src/config.rs`, `README.md` / `ARCHITECTURE.md`
- Test: `server/src/config.rs`

**Interfaces:**
- Consumes: the `host` config field (already present).
- Produces: `KrustConfig::load()` reads only the file; `is_origin_allowed` returns `true` whenever `host` is non-loopback.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn config_file_is_the_only_source_of_host_and_port() {
    std::env::set_var("HOST", "10.0.0.1");
    std::env::set_var("PORT", "9999");
    let config = KrustConfig::load();
    assert_eq!(config.host, "127.0.0.1");
    assert_eq!(config.port, 3000);
    std::env::remove_var("HOST");
    std::env::remove_var("PORT");
}

#[test]
fn non_loopback_host_allows_any_origin() {
    let config = KrustConfig { host: "0.0.0.0".to_string(), ..KrustConfig::default() };
    assert!(config.is_origin_allowed("http://192.168.1.9:3000"));
    let loopback = KrustConfig::default();
    assert!(!loopback.is_origin_allowed("http://192.168.1.9:3000"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib config::`
Expected: FAIL — env still overrides; LAN origin still rejected.

- [ ] **Step 3: Implement**

Delete the `HOST`/`PORT` block in `KrustConfig::load()` (`server/src/config.rs:55-63`). Make `is_origin_allowed` return `true` before consulting the list when `host` is not loopback. Correct the stale `0.0.0.0` bind diagram and add the config-is-authoritative note in the docs.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd ~/Projects/krust
git add server/src/config.rs README.md ARCHITECTURE.md
git commit -m "feat(config): config file is authoritative; allow any origin on LAN"
```

---

### Task 8: folio — configurable bind host

**Repo:** `~/Projects/folio`

**Files:**
- Modify: `src/config.rs`, `src/main.rs`, `ARCHITECTURE.md` / `README.md` / `CONTEXT.md`
- Test: `src/config.rs`

**Interfaces:**
- Consumes: the existing `bitshift/folio/config.json` file.
- Produces: `FolioConfig.host: String` (serde default `"127.0.0.1"`); `main` binds `(host, port)`; `save_to` merges `last_app` into the on-disk document instead of replacing it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn host_defaults_to_loopback_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    assert_eq!(load_from(&path).host, "127.0.0.1");
    let cfg = FolioConfig { host: "0.0.0.0".to_string(), ..Default::default() };
    save_to(&path, &cfg);
    assert_eq!(load_from(&path).host, "0.0.0.0");
}

#[test]
fn save_preserves_a_host_written_by_grit() {
    // grit sets `host`; picking an editor in folio must not revert it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, br#"{"host":"0.0.0.0","last_app":{}}"#).unwrap();
    let mut stale = load_from(&path);
    stale.last_app.insert("text/plain".into(), "zed.desktop".into());
    // The in-memory `host` is whatever folio read at startup; a concurrent
    // grit write must win over it.
    fs::write(&path, br#"{"host":"0.0.0.0","last_app":{}}"#).unwrap();
    save_to(&path, &stale);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["host"], "0.0.0.0");
    assert_eq!(value["last_app"]["text/plain"], "zed.desktop");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib config::`
Expected: FAIL — no `host` field.

- [ ] **Step 3: Implement**

Add `host` (serde default loopback) and rename `OpenWithConfig` → `FolioConfig` at its few call sites. `save_to` reads the on-disk document, sets only `last_app`, preserves every other key, then writes. `main.rs` loads the config and binds `(config.host.as_str(), args.port)`; keep `--port`. Update the docs that call loopback binding the security boundary.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd ~/Projects/folio
git add src/config.rs src/main.rs src/server/mod.rs ARCHITECTURE.md README.md CONTEXT.md
git commit -m "feat(config): configurable bind host, preserving grit's writes"
```

---

### Task 9: docks follow the page's host

**Files:**
- Modify: `web/dist/common.js:12,17`
- Test: `web/dist/*` syntax gate only (no JS test harness)

**Interfaces:**
- Produces: `KRUST_BASE` and `FOLIO_BASE` derived from `location.hostname`.

- [ ] **Step 1: Change the constants**

```js
// The docks are same-machine services: on a LAN browser `localhost` is the
// client, not the server, so both bases have to follow the page's own host.
export const KRUST_BASE = `http://${location.hostname}:3000`;
export const FOLIO_BASE = `http://${location.hostname}:4000`;
```

- [ ] **Step 2: Verify syntax**

Run: `cp web/dist/common.js /tmp/x.mjs && node --check /tmp/x.mjs`
Expected: no output (exit 0).

- [ ] **Step 3: Commit**

```bash
git add web/dist/common.js
git commit -m "fix(web): derive krust/folio bases from the page host"
```

---

### Task 10: gear button and settings page

**Files:**
- Modify: `web/dist/index.html`, `web/dist/style.css`, `web/dist/common.js`, `web/dist/terminal.js`, `web/dist/app.js`
- Create: `web/dist/settings.js`
- Test: syntax gate; manual check against a running daemon

**Interfaces:**
- Consumes: `GET /settings`, `POST /settings`, `POST /settings/restart` (Tasks 5, 6).
- Produces: `showView("settings")`; `renderTabBar` appends `<button class="tab settings-btn">`; `openSettings()` fetched on view entry.

- [ ] **Step 1: Add the settings view markup**

In `web/dist/index.html`, inside `<main>`, add:

```html
<section id="settings-view" class="settings-view" style="display: none;">
  <h2>Settings</h2>
  <p class="muted" id="settings-note"></p>
  <div id="settings-services"></div>
  <div class="form-actions">
    <button id="settings-restart-btn" style="display: none;">Restart krust &amp; folio</button>
    <span id="settings-restart-result" class="muted"></span>
  </div>
</section>
```

- [ ] **Step 2: Route the view**

In `web/dist/terminal.js` `showView`, treat `settings` like the other non-dashboard views: hide the five dashboard sections, `setDockVisible` both docks false, add `settings` to the body-class loop, and toggle `#settings-view`'s `display`. Extend the `?view=` list in `updateUrlView`/`initialUrlView`. In `web/dist/app.js`, extend the `#tabs` click delegation to handle `.settings-btn` → `setView("settings")`.

- [ ] **Step 3: Render the gear**

In `common.js` `renderTabBar`, after the `+` button, append the gear. It must have no `data-tab-id` (it is global) and must be pushed right:

```js
const gear = document.createElement("button");
gear.className = "tab settings-btn";
gear.textContent = "⚙";
gear.title = "Settings";
gear.setAttribute("aria-label", "Settings");
gear.style.marginLeft = "auto";
tabsEl.appendChild(gear);
```

- [ ] **Step 4: Implement `web/dist/settings.js`**

Fetch `/settings` when the view opens; render one row per service with a checkbox, the saved address, and for grit the effective address or error; show the restart button when any of folio/krust has `pending_restart`. A toggle posts `/settings` with that one key and re-renders from the response. The restart button posts `/settings/restart` and renders per-service results. `settings-note` carries one plain sentence: LAN access makes the service reachable by anything on the network, and krust is a shell.

- [ ] **Step 5: Verify syntax**

Run: `for f in common terminal app settings; do cp web/dist/$f.js /tmp/x.mjs && node --check /tmp/x.mjs || exit 1; done`
Expected: no output (exit 0).

- [ ] **Step 6: Rebuild and check by hand**

Run: `cargo build --release && cargo run -- --headless --port 5001` in one shell; open `http://127.0.0.1:5001`, click the gear, toggle grit on, confirm the page does not reload and `~/.config/bitshift/grit/config.json` gains `"host": "0.0.0.0"`.
Expected: the toggle persists, no page reload, no console errors.

- [ ] **Step 7: Commit**

```bash
git add web/dist/
git commit -m "feat(web): gear button and settings page"
```

---

### Task 11: document the trio's new LAN posture

**Files:**
- Modify: `AGENTS.md` (§2 grit/krust/folio entries, §4)

- [ ] **Step 1: Update the docs**

Record the settings page, the `host` field in all three config files, the live rebind for grit, the restart button and its `/proc` mechanism, and the fact that `KRUST_BASE`/`FOLIO_BASE` now follow `location.hostname`.

- [ ] **Step 2: Commit**

```bash
git add AGENTS.md
git commit -m "docs: LAN settings page and the three-app host config"
```
