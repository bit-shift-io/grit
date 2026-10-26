//! Cross-app settings: the bind host shared by grit, folio and krust.
//!
//! Each app keeps its own `config.json` under the same `bitshift/<app>` folder
//! and each honours a `host` field. Grit is the writer for all three: it owns
//! the settings page, and the sibling daemons read their file at startup.

use std::fs;
use std::path::{Path, PathBuf};

/// Loopback-only bind: LAN access off.
pub const LOOPBACK: &str = "127.0.0.1";
/// All-interfaces bind: LAN access on.
pub const ANY: &str = "0.0.0.0";

/// True when `host` is not a loopback literal.
pub fn is_lan(host: &str) -> bool {
    !matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// One of the three apps sharing the `bitshift/<app>` config scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Service {
    Grit,
    Folio,
    Krust,
}

impl Service {
    /// Every service, in the order the settings page lists them.
    pub const ALL: [Service; 3] = [Service::Grit, Service::Folio, Service::Krust];

    /// The two services the restart button owns. Grit applies its own bind
    /// live and is never restarted.
    pub const RESTARTABLE: [Service; 2] = [Service::Krust, Service::Folio];

    /// Stable machine key; also the field name in the settings JSON.
    pub fn key(self) -> &'static str {
        match self {
            Service::Grit => "grit",
            Service::Folio => "folio",
            Service::Krust => "krust",
        }
    }
}

/// Config file locations for the three apps (`None` when the environment has
/// neither `XDG_CONFIG_HOME` nor `HOME`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPaths {
    pub grit: Option<PathBuf>,
    pub krust: Option<PathBuf>,
    pub folio: Option<PathBuf>,
}

impl SettingsPaths {
    /// Resolves the three paths from the live environment.
    pub fn from_env() -> Self {
        resolve_paths(
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }
}

/// Pure form of [`SettingsPaths::from_env`], so the rules are testable without
/// touching the process environment.
///
/// Each app's own rule is mirrored exactly rather than unified: grit filters
/// `XDG_CONFIG_HOME` to absolute paths, krust and folio do not.
pub fn resolve_paths(xdg: Option<PathBuf>, home: Option<PathBuf>) -> SettingsPaths {
    let file = |base: Option<PathBuf>, app: &str| {
        base.map(|dir| dir.join("bitshift").join(app).join("config.json"))
    };
    let grit_base = xdg
        .clone()
        .filter(|p| p.is_absolute())
        .or_else(|| home.clone().map(|h| h.join(".config")));
    let other_base = xdg.or_else(|| home.map(|h| h.join(".config")));

    SettingsPaths {
        grit: file(grit_base, "grit"),
        krust: file(other_base.clone(), "krust"),
        folio: file(other_base, "folio"),
    }
}

/// The config path for one service, when the environment resolved one.
pub fn path_for(paths: &SettingsPaths, service: Service) -> Option<&PathBuf> {
    match service {
        Service::Grit => paths.grit.as_ref(),
        Service::Folio => paths.folio.as_ref(),
        Service::Krust => paths.krust.as_ref(),
    }
}

/// The host currently configured for one service.
pub fn host_for(paths: &SettingsPaths, service: Service) -> String {
    match path_for(paths, service) {
        Some(path) => read_host(path),
        None => LOOPBACK.to_string(),
    }
}

/// The host each service was last *started* with.
///
/// Grit applies its own toggle live, but folio and krust only read their config
/// at startup, so the page needs a remembered point of comparison to say
/// "this change still needs a restart".
#[derive(Debug)]
pub struct Baseline(std::sync::Mutex<std::collections::BTreeMap<Service, String>>);

impl Baseline {
    /// Snapshots every service's configured host as it is right now.
    pub fn capture(paths: &SettingsPaths) -> Self {
        let mut hosts = std::collections::BTreeMap::new();
        for service in Service::ALL {
            hosts.insert(service, host_for(paths, service));
        }
        Baseline(std::sync::Mutex::new(hosts))
    }

    /// The host this service is believed to be running with.
    pub fn get(&self, service: Service) -> String {
        lock(&self.0)
            .get(&service)
            .cloned()
            .unwrap_or_else(|| LOOPBACK.to_string())
    }

    /// Re-reads one service's host, after it has actually been restarted.
    pub fn recapture(&self, service: Service, host: String) {
        lock(&self.0).insert(service, host);
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Per-service state as the settings page sees it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceSettings {
    /// LAN access is on: the saved host is not loopback.
    pub lan: bool,
    /// The host saved in that service's config file.
    pub host: String,
    /// Grit only: the host actually bound right now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_host: Option<String>,
    /// Grit only: the last bind failure, if a switch did not take.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Folio and krust only: the saved host is not the one they started with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_restart: Option<bool>,
}

/// The ports the settings page shows for each service.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Ports {
    pub grit: u16,
    pub folio: u16,
    pub krust: u16,
}

impl Ports {
    /// Grit's own port, with the two daemons' fixed ports.
    pub fn for_grit_port(grit: u16) -> Self {
        Ports {
            grit,
            folio: crate::folio::FOLIO_PORT,
            krust: crate::krust::KRUST_PORT,
        }
    }
}

/// The whole payload of `GET /settings`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SettingsResponse {
    pub grit: ServiceSettings,
    pub folio: ServiceSettings,
    pub krust: ServiceSettings,
    pub ports: Ports,
}

/// The body of `POST /settings`: only the services the user touched.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SettingsUpdate {
    #[serde(default)]
    pub grit: Option<bool>,
    #[serde(default)]
    pub folio: Option<bool>,
    #[serde(default)]
    pub krust: Option<bool>,
}

fn service_settings(
    paths: &SettingsPaths,
    service: Service,
    control: &crate::server::ServerControl,
    baseline: &Baseline,
) -> ServiceSettings {
    let host = host_for(paths, service);
    let lan = is_lan(&host);
    if service == Service::Grit {
        return ServiceSettings {
            lan,
            host,
            effective_host: Some(control.effective()),
            error: control.last_error(),
            pending_restart: None,
        };
    }
    let pending = baseline.get(service) != host;
    ServiceSettings {
        lan,
        host,
        effective_host: None,
        error: None,
        pending_restart: Some(pending),
    }
}

/// Builds the settings response the page renders.
pub fn snapshot(
    paths: &SettingsPaths,
    control: &crate::server::ServerControl,
    baseline: &Baseline,
    ports: Ports,
) -> SettingsResponse {
    // Report exactly the port the restart would act on, so the page can never
    // advertise one thing and the button do another.
    let ports = Ports {
        krust: port_for(paths, Service::Krust).unwrap_or(crate::krust::KRUST_PORT),
        ..ports
    };
    SettingsResponse {
        grit: service_settings(paths, Service::Grit, control, baseline),
        folio: service_settings(paths, Service::Folio, control, baseline),
        krust: service_settings(paths, Service::Krust, control, baseline),
        ports,
    }
}

/// Writes the requested toggles to each service's config file and moves grit's
/// own listener.
///
/// Grit's rebind is requested even when its config path could not be resolved:
/// refusing to honour the user's choice because the file is unwritable would be
/// worse than applying it for this session.
pub fn apply(
    paths: &SettingsPaths,
    update: &SettingsUpdate,
    control: &crate::server::ServerControl,
) -> Result<(), String> {
    let requested = [
        (Service::Grit, update.grit),
        (Service::Folio, update.folio),
        (Service::Krust, update.krust),
    ];
    for (service, lan) in requested {
        let Some(lan) = lan else { continue };
        let host = if lan { ANY } else { LOOPBACK };
        if let Some(path) = path_for(paths, service) {
            write_host(path, host).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if service == Service::Grit {
            control.request_host(host.to_string());
        }
    }
    Ok(())
}

/// How long a SIGTERMed daemon gets to release its port before it is killed.
const TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a freshly spawned daemon gets to start listening.
const START_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// The port a service listens on. Krust's comes from its config file, so the
/// restart acts on exactly the port the settings page advertises rather than
/// silently looking at the default.
pub fn port_for(paths: &SettingsPaths, service: Service) -> Option<u16> {
    match service {
        Service::Krust => Some(
            path_for(paths, Service::Krust)
                .map(|path| read_port(path, crate::krust::KRUST_PORT))
                .unwrap_or(crate::krust::KRUST_PORT),
        ),
        Service::Folio => Some(crate::folio::FOLIO_PORT),
        Service::Grit => None,
    }
}

/// Listen rows of a `/proc/net/tcp` body: `(port, socket inode)`.
pub fn parse_listening_inodes(net_tcp: &str) -> Vec<(u16, u64)> {
    net_tcp
        .lines()
        .skip(1) // header
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // sl local_address rem_address st ... inode (index 9)
            if fields.get(3) != Some(&"0A") {
                return None; // 0A is the only listening state
            }
            let port = u16::from_str_radix(fields.get(1)?.split(':').nth(1)?, 16).ok()?;
            let inode = fields.get(9)?.parse::<u64>().ok()?;
            Some((port, inode))
        })
        .collect()
}

/// The pid listening on `port`, when it can be found.
///
/// Only pids reached through their own socket fd are returned: the caller
/// signals whatever this hands back, so guessing would risk killing an
/// unrelated process.
pub fn find_pid_for_port(net_tcp: &str, port: u16) -> Option<i32> {
    let inode = parse_listening_inodes(net_tcp)
        .into_iter()
        .find(|(p, _)| *p == port)
        .map(|(_, inode)| inode)?;
    let needle = format!("socket:[{inode}]");

    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                if target.to_string_lossy() == needle {
                    return Some(pid);
                }
            }
        }
    }
    None
}

/// The pid currently listening on `port`, read from the live `/proc`.
pub fn listening_pid(port: u16) -> Option<i32> {
    let net_tcp = std::fs::read_to_string("/proc/net/tcp").unwrap_or_default();
    find_pid_for_port(&net_tcp, port)
}

/// The executable's file name behind a pid, from `/proc/<pid>/exe`.
pub fn process_exe_name(pid: i32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(exe.file_name()?.to_string_lossy().into_owned())
}

/// True when an executable name belongs to `service`.
///
/// Deliberately exact. The restart signals whatever holds the port, and 3000
/// and 4000 are the canonical dev-server ports, so a loose match would turn
/// "restart my terminal" into "kill your Vite server" — or, if grit itself was
/// started with `--port 3000`, into "kill grit".
pub fn exe_matches_service(exe: &str, service: Service) -> bool {
    let Some(name) = std::path::Path::new(exe).file_name() else {
        return false;
    };
    let name = name.to_string_lossy();
    match service {
        Service::Krust => name == "krust" || name == "krust.exe",
        Service::Folio => name == "folio" || name == "folio.exe",
        Service::Grit => false,
    }
}

/// True when `pid` is running `service`'s binary.
pub fn pid_matches_service(pid: i32, service: Service) -> bool {
    process_exe_name(pid).is_some_and(|exe| exe_matches_service(&exe, service))
}

/// Polls until something accepts connections on `port`, or `timeout` elapses.
pub async fn wait_for_listening(port: u16, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Sends `signal` to `pid`, which always comes from [`find_pid_for_port`] and
/// has been confirmed by [`pid_matches_service`].
#[cfg(unix)]
fn signal(pid: i32, signal: i32) {
    // SAFETY: kill(2) takes no pointers and cannot violate aliasing; the return
    // value is deliberately ignored because a daemon that died on its own is
    // indistinguishable from one we asked to stop.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// Blocking half of a restart: stop the daemon holding `port`.
///
/// Returns whether anything was stopped. Runs on a blocking thread: the `/proc`
/// scan walks every process's file descriptors, and re-scans while waiting for
/// the port to free.
#[cfg(unix)]
fn stop_service(port: u16, service: Service) -> Result<bool, String> {
    let Some(pid) = listening_pid(port) else {
        return Ok(false);
    };
    if !pid_matches_service(pid, service) {
        return Err(format!(
            "port {port} is held by pid {pid}, which is not {}",
            service.key()
        ));
    }

    tracing::info!("stopping {} (pid {pid}) on port {port}", service.key());
    signal(pid, libc::SIGTERM);
    let deadline = std::time::Instant::now() + TERM_GRACE;
    while std::time::Instant::now() < deadline && listening_pid(port).is_some() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Some(pid) = listening_pid(port) {
        tracing::warn!("{pid} did not release port {port}; killing it");
        signal(pid, libc::SIGKILL);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(true)
}

#[cfg(not(unix))]
fn stop_service(_port: u16, _service: Service) -> Result<bool, String> {
    Err("restarting a daemon is only supported on unix".to_string())
}

/// Restarts one daemon so its freshly written config takes effect.
///
/// Grit never owns these processes — the user usually starts them from a shell
/// — so the pid is discovered from the port they are listening on. Nothing
/// listening is not an error: it just means the daemon needs starting.
pub async fn restart_service(
    app: &crate::server::AppState,
    service: Service,
) -> Result<(), String> {
    let Some(port) = port_for(&app.settings_paths, service) else {
        return Err("grit rebinds its own listener live and is never restarted".to_string());
    };

    tokio::task::spawn_blocking(move || stop_service(port, service))
        .await
        .map_err(|e| format!("restart task panicked: {e}"))??;

    match service {
        Service::Krust => crate::krust::ensure_krust().await,
        Service::Folio => {
            let root = app
                .registry
                .snapshot()
                .tabs
                .first()
                .map(|t| t.repo_path.clone())
                .filter(|p| !p.is_empty());
            crate::folio::ensure_folio(root.as_deref().map(std::path::Path::new)).await;
        }
        Service::Grit => unreachable!("port_for returns None for grit"),
    }

    // `ensure_*` swallows its own failures, so the port is the only honest
    // answer: reporting "restarted" for a daemon that never came up (because
    // the binary is missing, say) is worse than reporting the failure.
    if !wait_for_listening(port, START_GRACE).await {
        return Err(format!(
            "{} is not listening on {port}; is it installed?",
            service.key()
        ));
    }

    // Whatever it just started with is the new baseline for `pending_restart`.
    app.baseline
        .recapture(service, host_for(&app.settings_paths, service));
    Ok(())
}

/// Reads a sibling app's configured host, defaulting to loopback when the file
/// is absent, unparseable, or carries no usable `host`.
pub fn read_host(path: &Path) -> String {
    let Ok(bytes) = fs::read(path) else {
        return LOOPBACK.to_string();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return LOOPBACK.to_string();
    };
    value
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| LOOPBACK.to_string())
}

/// Reads a numeric field (krust's `port`), defaulting when absent or unusable.
pub fn read_port(path: &Path, default: u16) -> u16 {
    let Ok(bytes) = fs::read(path) else {
        return default;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return default;
    };
    value
        .get("port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(default)
}

/// Sets `host` in a config file, preserving every other field.
///
/// The file is treated as an opaque JSON object so the fields grit does not own
/// (krust's `port`/`allowed_origins`, folio's `last_app`) survive. A document
/// that cannot be parsed is left untouched and reported: a corrupt file is not
/// ours to overwrite.
pub fn write_host(path: &Path, host: &str) -> std::io::Result<()> {
    // Same lock grit's own tab persistence uses; a lost update between the two
    // would drop either the tabs or the host.
    let _guard = crate::shared_config::config_write_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut value = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is not valid JSON: {e}", path.display()),
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(e) => return Err(e),
    };
    if !value.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not a JSON object", path.display()),
        ));
    }
    value["host"] = serde_json::Value::String(host.to_string());

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn paths_mirror_each_apps_own_rule() {
        let paths = resolve_paths(Some(PathBuf::from("/xdg")), Some(PathBuf::from("/home/u")));
        assert_eq!(paths.grit, Some(PathBuf::from("/xdg/bitshift/grit/config.json")));
        assert_eq!(paths.krust, Some(PathBuf::from("/xdg/bitshift/krust/config.json")));
        // No XDG: every app falls back to $HOME/.config with the same tail.
        let paths = resolve_paths(None, Some(PathBuf::from("/home/u")));
        assert_eq!(
            paths.folio,
            Some(PathBuf::from("/home/u/.config/bitshift/folio/config.json"))
        );
    }

    #[test]
    fn write_host_preserves_fields_grit_does_not_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            br#"{"host":"127.0.0.1","port":3000,"allowed_origins":["http://localhost:3000"]}"#,
        )
        .unwrap();
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
    fn parse_listening_inodes_reads_only_listen_rows() {
        let fixture = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:C350 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0
   1: 0100007F:C351 00000000:0000 01 00000000:00000000 00:00000000 00000000  1000        0 99999 1 0000000000000000 100 0 0 10 0
";
        // 0xC350 == 50000, state 0A == LISTEN; the second row is ESTABLISHED.
        assert_eq!(parse_listening_inodes(fixture), vec![(50000, 12345)]);
    }

    #[test]
    fn find_pid_for_port_finds_nothing_for_an_idle_port() {
        // Port 1 is never listened on by these tests, so the scan has to return
        // None rather than some unrelated pid.
        let net_tcp = fs::read_to_string("/proc/net/tcp").unwrap_or_default();
        assert!(find_pid_for_port(&net_tcp, 1).is_none());
    }

    #[test]
    fn exe_matches_service_only_for_the_named_binary() {
        assert!(exe_matches_service("/usr/bin/krust", Service::Krust));
        assert!(exe_matches_service("/home/u/.local/bin/krust", Service::Krust));
        assert!(exe_matches_service("/opt/krust/krust.exe", Service::Krust));
        assert!(!exe_matches_service("/usr/bin/node", Service::Krust));
        assert!(exe_matches_service("/usr/bin/folio", Service::Folio));
        assert!(!exe_matches_service("/usr/bin/krust", Service::Folio));
        // grit's own binary must never look like a restartable service: running
        // grit on port 3000 must not make the button kill grit.
        assert!(!exe_matches_service("/home/u/.local/bin/grit", Service::Krust));
        assert!(!exe_matches_service("/home/u/.local/bin/grit", Service::Folio));
    }

    #[test]
    fn our_own_process_is_not_a_restartable_service() {
        let me = std::process::id() as i32;
        assert!(process_exe_name(me).is_some(), "should read /proc/<pid>/exe");
        assert!(!pid_matches_service(me, Service::Krust));
        assert!(!pid_matches_service(me, Service::Folio));
    }

    #[test]
    fn port_for_agrees_with_the_port_the_page_reports() {
        let dir = tempfile::tempdir().unwrap();
        let krust_path = dir.path().join("krust.json");
        fs::write(&krust_path, br#"{"port":4123}"#).unwrap();
        let paths = SettingsPaths {
            grit: None,
            krust: Some(krust_path),
            folio: None,
        };
        // The restart has to act on the port the page advertises, or it looks
        // at 3000 while showing something else.
        assert_eq!(port_for(&paths, Service::Krust), Some(4123));
        assert_eq!(port_for(&paths, Service::Folio), Some(crate::folio::FOLIO_PORT));
        assert_eq!(port_for(&paths, Service::Grit), None);
    }

    #[tokio::test]
    async fn wait_for_listening_sees_a_bound_port_and_gives_up_on_a_closed_one() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let open = listener.local_addr().unwrap().port();
        assert!(wait_for_listening(open, std::time::Duration::from_millis(500)).await);

        let closed = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(!wait_for_listening(closed, std::time::Duration::from_millis(150)).await);
    }

    #[test]
    fn find_pid_for_port_finds_our_own_listener() {
        // The safety property that matters: a port we are listening on maps to
        // *our* pid, so the restart can never signal somebody else.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let net_tcp = fs::read_to_string("/proc/net/tcp").unwrap();
        assert_eq!(
            find_pid_for_port(&net_tcp, port),
            Some(std::process::id() as i32)
        );
    }

    #[test]
    fn read_host_defaults_to_loopback_when_absent_or_invalid() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_host(&dir.path().join("missing.json")), LOOPBACK);
        let bad = dir.path().join("bad.json");
        fs::write(&bad, b"not json").unwrap();
        assert_eq!(read_host(&bad), LOOPBACK);
    }
}
