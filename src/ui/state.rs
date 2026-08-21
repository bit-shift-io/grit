//! Iced application state, message dispatch, and background-task wiring.
//!
//! The desktop GUI is a pure client of the shared [`TabRegistry`]: every tab
//! mutation (opening/closing repositories) goes through the same server-side
//! operations the web UI uses, and the local tab list is derived exclusively
//! from `WebTabsSync` deliveries. The GUI never writes the registry's tab
//! list and never persists configuration — the server owns both.

use std::path::PathBuf;

use iced::widget::{button, column, row, rule, text, text_input};
use iced::{Element, Length, Subscription, Task};

use crate::git::types::{GitAction, RepoState};
use crate::ui::components;

/// UI events produced by widgets and background tasks.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    // Tab management.
    AddTabPressed,
    OpenTab(usize),
    CloseTab(usize),
    CancelAddForm,
    NewRepoNameChanged(String),
    NewRepoPathChanged(String),
    BrowseFolder,
    FolderPicked(Option<PathBuf>),
    OpenNewRepo,
    /// Outcome of the shared `open_repo_tab` operation.
    OpenRepoResult(Result<usize, String>),
    /// Fire-and-forget result for operations whose effect arrives via sync.
    Nop,
    // Git actions applied to the active repository tab.
    StageFile(String),
    UnstageFile(String),
    CommitPressed,
    CommitMessageChanged(String),
    PushPressed,
    PullPressed,
    CheckoutBranch(String),
    Revert(String),
    ShowDiff(String),
    DiffLoaded(String, String),
    NukePressed,
    // Watcher / background updates, routed by tab id.
    TabRefresh(usize),
    TabStateUpdated(usize, RepoState),
    TabError(usize, String),
    /// Mirror of the shared registry's tab list; the sole source of truth
    /// for which repository tabs exist.
    WebTabsSync(Vec<crate::server::registry::WebTab>),
}

/// A configured, live repository tab.
#[derive(Debug, Clone)]
pub struct RepoTab {
    id: usize,
    name: String,
    repo_path: PathBuf,
    repo_state: RepoState,
    commit_message: String,
    diff: Option<String>,
    nuke_armed: bool,
    error: Option<String>,
}

/// Root application state for the native desktop UI.
#[derive(Debug, Clone)]
pub struct GritApp {
    /// Mirrors the shared registry; mutated only through `apply_sync`.
    tabs: Vec<RepoTab>,
    active: usize,
    registry: Option<crate::server::registry::TabRegistry>,
    /// Local "+" form mode. The form is never a tab; with zero tabs it is
    /// also the default view.
    show_add_form: bool,
    add_name: String,
    add_path: String,
    add_error: Option<String>,
    /// Remote (connect) mode: port of the running daemon to attach to.
    /// `None` means embedded mode with the local registry.
    remote_port: Option<u16>,
    /// Remote mode only: repo path from an explicit `--path` that must be
    /// opened on the daemon once the first sync arrives.
    pending_open: Option<String>,
}

impl GritApp {
    /// Creates an empty app; tab contents arrive via `WebTabsSync`.
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            registry: None,
            show_add_form: false,
            add_name: String::new(),
            add_path: String::new(),
            add_error: None,
            remote_port: None,
            pending_open: None,
        }
    }

    /// Whether the Add Repository form should be displayed.
    fn showing_add_form(&self) -> bool {
        self.show_add_form || self.tabs.is_empty()
    }

    fn active_repo(&self) -> Option<&RepoTab> {
        self.tabs.get(self.active)
    }

    fn active_repo_mut(&mut self) -> Option<&mut RepoTab> {
        self.tabs.get_mut(self.active)
    }

    fn index_of_id(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Applies a registry snapshot to the local tab list: adopts new tabs
    /// (preserving per-tab UI fields for known ids), drops removed ones,
    /// and heals the selection. This is the ONLY place `self.tabs` changes
    /// outside of synchronous form handling.
    fn apply_sync(&mut self, live_tabs: &[crate::server::registry::WebTab]) {
        let live_ids: std::collections::HashSet<usize> =
            live_tabs.iter().map(|t| t.id).collect();

        let before = self.tabs.len();
        self.tabs.retain(|r| live_ids.contains(&r.id));
        let mut changed = self.tabs.len() != before;

        let mut newly_adopted: Option<usize> = None;
        for web in live_tabs.iter().filter(|t| !t.repo_path.is_empty()) {
            if let Some(existing) = self.tabs.iter_mut().find(|r| r.id == web.id) {
                // Server owns identity fields; local UI fields are kept.
                existing.name = web.name.clone();
                existing.repo_path = PathBuf::from(&web.repo_path);
                existing.repo_state = web.state.clone();
                continue;
            }
            self.tabs.push(RepoTab {
                id: web.id,
                name: web.name.clone(),
                repo_path: PathBuf::from(&web.repo_path),
                repo_state: web.state.clone(),
                commit_message: String::new(),
                diff: None,
                nuke_armed: false,
                error: None,
            });
            newly_adopted = Some(web.id);
            changed = true;
        }

        if !changed {
            return;
        }

        // Follow newly created tabs and hide the "+" form once a repo exists.
        if let Some(id) = newly_adopted {
            if let Some(pos) = self.index_of_id(id) {
                self.active = pos;
            }
            self.show_add_form = false;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AddTabPressed => {
                self.show_add_form = true;
                Task::none()
            }
            Message::CancelAddForm => {
                self.show_add_form = false;
                self.add_error = None;
                Task::none()
            }
            Message::OpenTab(index) => {
                if index < self.tabs.len() {
                    self.active = index;
                    self.show_add_form = false;
                }
                Task::none()
            }
            Message::CloseTab(index) => {
                let Some(tab) = self.tabs.get(index) else {
                    return Task::none();
                };
                let id = tab.id;
                if let Some(port) = self.remote_port {
                    // Remote mode: ask the daemon to close; the echo sync
                    // performs the local removal.
                    return Self::remote_op(port, None, crate::git::types::GitAction::CloseTab);
                }
                match self.registry.clone() {
                    Some(registry) => Task::perform(
                        async move {
                            crate::server::websocket::close_tab_by_id(&registry, id);
                            Message::Nop
                        },
                        |m| m,
                    ),
                    None => {
                        // No shared workspace (standalone): remove locally.
                        self.tabs.remove(index);
                        if self.active >= self.tabs.len() {
                            self.active = self.tabs.len().saturating_sub(1);
                        }
                        Task::none()
                    }
                }
            }
            Message::NewRepoNameChanged(name) => {
                self.add_name = name;
                Task::none()
            }
            Message::NewRepoPathChanged(path) => {
                self.add_path = path;
                Task::none()
            }
            Message::BrowseFolder => Task::perform(
                async move {
                    rfd::FileDialog::new()
                        .set_title("Select a Git repository folder")
                        .pick_folder()
                },
                Message::FolderPicked,
            ),
            Message::FolderPicked(picked) => {
                if let Some(path) = picked {
                    self.add_path = path.display().to_string();
                }
                Task::none()
            }
            Message::OpenNewRepo => {
                let name = self.add_name.trim().to_string();
                let path = self.add_path.trim().to_string();
                // Instant client-side validation; the shared op re-validates.
                let validation_error = if path.is_empty() {
                    Some("Folder path is required".to_string())
                } else if !PathBuf::from(&path).is_dir() {
                    Some(format!("Not a directory: {path}"))
                } else {
                    None
                };
                if let Some(error) = validation_error {
                    self.add_error = Some(error);
                    return Task::none();
                }
                if let Some(port) = self.remote_port {
                    // Remote mode: the daemon opens the repo; adoption via
                    // sync clears the form.
                    let payload = serde_json::json!({ "name": name, "path": path }).to_string();
                    return Self::remote_op(
                        port,
                        None,
                        crate::git::types::GitAction::NewTab(payload),
                    );
                }
                match self.registry.clone() {
                    Some(registry) => Task::perform(
                        async move {
                            Message::OpenRepoResult(
                                crate::server::websocket::open_repo_tab(
                                    &registry, name, path,
                                )
                                .await,
                            )
                        },
                        |m| m,
                    ),
                    None => {
                        self.add_error =
                            Some("No workspace available".to_string());
                        Task::none()
                    }
                }
            }
            Message::OpenRepoResult(result) => {
                match result {
                    Ok(id) => {
                        // The form resets for its next use; the tab itself
                        // arrives through WebTabsSync adoption.
                        self.show_add_form = false;
                        self.add_name.clear();
                        self.add_path.clear();
                        self.add_error = None;
                        if let Some(tab) = self.tabs.iter().find(|r| r.id == id) {
                            let path = tab.repo_path.clone();
                            return refresh(id, path);
                        }
                    }
                    Err(error) => {
                        self.add_error = Some(error);
                    }
                }
                Task::none()
            }
            // Git actions on the active repository tab.
            Message::StageFile(path) => self.run_action(GitAction::Stage(path)),
            Message::UnstageFile(path) => self.run_action(GitAction::Unstage(path)),
            Message::CommitPressed => {
                let Some(tab) = self.active_repo_mut() else {
                    return Task::none();
                };
                let message = tab.commit_message.trim().to_string();
                if message.is_empty() {
                    tab.error = Some("Commit message must not be empty".to_string());
                    return Task::none();
                }
                tab.commit_message.clear();
                let id = tab.id;
                let repo_path = tab.repo_path.clone();
                run_action_on(id, repo_path, GitAction::Commit(message))
            }
            Message::CommitMessageChanged(value) => {
                if let Some(tab) = self.active_repo_mut() {
                    tab.commit_message = value;
                }
                Task::none()
            }
            Message::PushPressed => self.run_action(GitAction::Push),
            Message::PullPressed => self.run_action(GitAction::Pull),
            Message::CheckoutBranch(branch) => self.run_action(GitAction::CheckoutBranch(branch)),
            Message::Revert(hash) => self.run_action(GitAction::Revert(hash)),
            Message::ShowDiff(path) => {
                let Some(tab) = self.active_repo() else {
                    return Task::none();
                };
                let repo_path = tab.repo_path.clone();
                let id = tab.id;
                Task::perform(
                    async move {
                        match crate::git::get_file_diff(&repo_path, &path) {
                            Ok(diff) => Message::DiffLoaded(path, diff),
                            Err(e) => Message::TabError(id, e.to_string()),
                        }
                    },
                    |m| m,
                )
            }
            Message::DiffLoaded(path, diff) => {
                if let Some(tab) = self.active_repo_mut() {
                    if tab.repo_state.changes.iter().any(|c| c.path == path) {
                        tab.diff = Some(diff);
                    }
                }
                Task::none()
            }
            Message::NukePressed => {
                let Some(tab) = self.active_repo_mut() else {
                    return Task::none();
                };
                if tab.nuke_armed {
                    tab.nuke_armed = false;
                    let id = tab.id;
                    let repo_path = tab.repo_path.clone();
                    run_action_on(id, repo_path, GitAction::Nuke)
                } else {
                    tab.nuke_armed = true;
                    Task::none()
                }
            }
            // Watcher and background updates, routed by tab id.
            Message::TabRefresh(id) => {
                let path = self.tabs.iter().find_map(|t| {
                    if t.id == id {
                        Some(t.repo_path.clone())
                    } else {
                        None
                    }
                });
                match path {
                    Some(path) => refresh(id, path),
                    None => Task::none(),
                }
            }
            Message::TabStateUpdated(id, state) => {
                if let Some(tab) = self.tabs.iter_mut().find(|r| r.id == id) {
                    tab.repo_state = state;
                    tab.nuke_armed = false;
                    tab.error = None;
                }
                Task::none()
            }
            Message::TabError(id, error) => {
                if let Some(tab) = self.tabs.iter_mut().find(|r| r.id == id) {
                    tab.error = Some(error);
                }
                Task::none()
            }
            Message::WebTabsSync(live_tabs) => {
                self.apply_sync(&live_tabs);
                // Remote startup: open an explicitly requested repo once the
                // daemon's tab list is known.
                if let (Some(port), Some(path)) = (self.remote_port, self.pending_open.take()) {
                    if !live_tabs.iter().any(|t| t.repo_path == path) {
                        let payload =
                            serde_json::json!({ "name": "", "path": path }).to_string();
                        return Self::remote_op(
                            port,
                            None,
                            crate::git::types::GitAction::NewTab(payload),
                        );
                    }
                }
                Task::none()
            }
            Message::Nop => Task::none(),
        }
    }

    /// Runs a git action against the active tab on an executor worker thread.
    fn run_action(&self, action: GitAction) -> Task<Message> {
        let Some(tab) = self.active_repo() else {
            return Task::none();
        };
        if let Some(port) = self.remote_port {
            // Remote mode: the daemon executes the action on its copy.
            return Self::remote_op(port, Some(tab.id), action);
        }
        run_action_on(tab.id, tab.repo_path.clone(), action)
    }

    /// Fire-and-forget dispatch of a client message to the daemon.
    fn remote_op(port: u16, tab: Option<usize>, action: GitAction) -> Task<Message> {
        let json = crate::server::websocket::encode_client_message(tab, &action);
        Task::perform(crate::ui::remote::send_op(port, json), |_| Message::Nop)
    }

    /// Watches every open repository and emits per-tab refresh events.
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs: Vec<Subscription<Message>> = self
            .tabs
            .iter()
            .map(|tab| watcher_subscription(tab.id, tab.repo_path.clone()))
            .collect();
        if let Some(port) = self.remote_port {
            subs.push(remote_subscription(port));
        } else if self.registry.is_some() {
            subs.push(registry_subscription());
        }
        Subscription::batch(subs)
    }

    pub fn view(&self) -> Element<'_, Message> {
        column![self.tab_bar(), rule::horizontal(1), self.tab_content()]
            .padding(12)
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn tab_bar(&self) -> Element<'_, Message> {
        let mut children: Vec<Element<'_, Message>> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let is_active = index == self.active && !self.showing_add_form();
                let tab_button = button(text(tab.name.clone()))
                    .on_press(Message::OpenTab(index))
                    .style(tab_button_style(is_active));
                row![tab_button, button(text("×")).on_press(Message::CloseTab(index))]
                    .spacing(2)
                    .into()
            })
            .collect();
        children.push(button(text("+").size(16)).on_press(Message::AddTabPressed).into());
        row(children).spacing(4).into()
    }

    fn tab_content(&self) -> Element<'_, Message> {
        if self.showing_add_form() {
            return add_repo_view(&self.add_name, &self.add_path, self.add_error.as_ref());
        }
        match self.tabs.get(self.active) {
            Some(tab) => self.repo_view(tab),
            None => text("No tabs").into(),
        }
    }

    fn repo_view<'a>(&self, tab: &'a RepoTab) -> Element<'a, Message> {
        let error_bar = if let Some(error) = &tab.error {
            text(format!("Error: {error}"))
                .color(iced::Color::from_rgb(0.9, 0.25, 0.25))
        } else {
            text("")
        };

        column![
            components::header::header(&tab.repo_state, tab.nuke_armed),
            error_bar,
            components::staging::staging(&tab.repo_state.changes),
            components::diff::diff(&tab.diff),
            components::commit::commit(&tab.commit_message),
            rule::horizontal(1),
            components::history::history(&tab.repo_state.history),
        ]
        .spacing(10)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Default for GritApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Style for a tab-bar button: highlighted when it is the active tab.
fn tab_button_style(
    is_active: bool,
) -> impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |theme, status| {
        let palette = theme.palette();
        let mut style = iced::widget::button::Style::default();
        let hovered = matches!(status, iced::widget::button::Status::Hovered);
        style.background = Some(iced::Background::Color(if is_active {
            palette.primary
        } else if hovered {
            palette.background
        } else {
            palette.background
        }));
        style.text_color = if is_active {
            palette.background
        } else {
            palette.text
        };
        style
    }
}

/// Form for opening a new repository into the shared workspace.
fn add_repo_view<'a>(
    name: &'a str,
    path: &'a str,
    error: Option<&'a String>,
) -> Element<'a, Message> {
    let error_bar = if let Some(error) = error {
        text(format!("Error: {error}"))
            .color(iced::Color::from_rgb(0.9, 0.25, 0.25))
    } else {
        text("")
    };

    column![
        text("Add Repository").size(20),
        text("Tab name"),
        text_input("e.g. my-project", name).on_input(Message::NewRepoNameChanged),
        text("Folder path"),
        row![
            text_input("Path to the repository folder", path)
                .on_input(Message::NewRepoPathChanged)
                .width(Length::Fill),
            button("Browse…").on_press(Message::BrowseFolder),
        ]
        .spacing(6),
        error_bar,
        row![
            button("Open Repository").on_press(Message::OpenNewRepo),
            button("Cancel").on_press(Message::CancelAddForm),
        ]
        .spacing(8),
    ]
    .spacing(8)
    .width(Length::Fill)
    .into()
}

/// Watches one repository and emits refresh events for its tab.
fn watcher_subscription(id: usize, repo_path: PathBuf) -> Subscription<Message> {
    Subscription::run_with((id, repo_path), |data| {
        let id = data.0;
        let path = data.1.clone();
        let (mut tx, rx) = futures_channel::mpsc::channel::<Message>(100);
        std::thread::spawn(move || {
            let (watcher_tx, mut watcher_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let watcher = match crate::git::watcher::spawn_watcher(path, watcher_tx) {
                Ok(w) => w,
                Err(e) => {
                    let _ = tx.try_send(Message::TabError(id, format!("watcher failed: {e}")));
                    return;
                }
            };
            let _keep_alive = watcher;
            while watcher_rx.blocking_recv().is_some() {
                if tx.try_send(Message::TabRefresh(id)).is_err() {
                    break;
                }
            }
        });
        rx
    })
}

/// Shared-registry handle readable from the subscription's fn-pointer builder.
static SHARED_REGISTRY: std::sync::OnceLock<crate::server::registry::TabRegistry> =
    std::sync::OnceLock::new();

/// Streams shared-registry snapshots into the GUI. This is how tabs opened
/// or closed anywhere (web or desktop) reach every view.
/// Streams daemon tab state into the GUI in remote (connect) mode.
fn remote_subscription(port: u16) -> Subscription<Message> {
    Subscription::run_with(port, |p| {
        let port = *p;
        let (mut tx, rx) = futures_channel::mpsc::channel::<Message>(100);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(rt) = rt else {
                return;
            };
            rt.block_on(async move {
                let (sync_tx, mut sync_rx) = tokio::sync::mpsc::unbounded_channel::<
                    Vec<crate::server::registry::WebTab>,
                >();
                let client = tokio::spawn(crate::ui::remote::run_client(port, sync_tx));
                while let Some(tabs) = sync_rx.recv().await {
                    if tx.try_send(Message::WebTabsSync(tabs)).is_err() {
                        break;
                    }
                }
                client.abort();
            });
        });
        rx
    })
}
fn registry_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        let (mut tx, rx) = futures_channel::mpsc::channel::<Message>(100);
        if let Some(registry) = SHARED_REGISTRY.get() {
            let registry = registry.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let Ok(rt) = rt else {
                    return;
                };
                rt.block_on(async move {
                    let mut watch_rx = registry.subscribe();
                    loop {
                        if watch_rx.changed().await.is_err() {
                            break;
                        }
                        let tabs = watch_rx.borrow().tabs.clone();
                        if tx.try_send(Message::WebTabsSync(tabs)).is_err() {
                            break;
                        }
                    }
                });
            });
        }
        rx
    })
}

/// Recomputes repository status on an executor worker thread.
fn refresh(id: usize, repo_path: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            match crate::git::get_repository_status(&repo_path) {
                Ok(state) => Message::TabStateUpdated(id, state),
                Err(e) => Message::TabError(id, e.to_string()),
            }
        },
        |m| m,
    )
}

/// Executes a git action on an executor worker thread, then refreshes.
fn run_action_on(id: usize, repo_path: PathBuf, action: GitAction) -> Task<Message> {
    Task::perform(
        async move {
            match crate::git::execute_action(&repo_path, action) {
                Ok(()) => Message::TabRefresh(id),
                Err(e) => Message::TabError(id, e.to_string()),
            }
        },
        |m| m,
    )
}

/// How the desktop GUI obtains its tab list.
#[derive(Debug, Clone)]
pub enum GuiMode {
    /// Owns an embedded server writing to this local registry.
    Embedded(crate::server::registry::TabRegistry),
    /// Attaches to an already-running daemon over WebSocket.
    Remote { port: u16 },
}

/// Launches the native GUI as a pure client of the shared registry.
///
/// With an explicit `--path`, the repository is opened through the shared
/// operation so the resulting tab is visible to every connected client.
pub fn run(mode: GuiMode, repo_path: PathBuf, open_explicit: bool) -> iced::Result {
    let mut app = GritApp::new();
    let mut registry_for_startup: Option<crate::server::registry::TabRegistry> = None;
    match &mode {
        GuiMode::Embedded(registry) => {
            let _ = SHARED_REGISTRY.set(registry.clone());
            app.registry = Some(registry.clone());
            // Seed from the current snapshot (boot may have restored saved
            // repos already); later changes arrive through the subscription.
            app.apply_sync(&registry.snapshot().tabs);
            registry_for_startup = Some(registry.clone());
        }
        GuiMode::Remote { port } => {
            app.remote_port = Some(*port);
            if open_explicit {
                // Deferred until the first sync: the daemon may still be
                // starting (systemd boot race).
                app.pending_open = Some(repo_path.display().to_string());
            }
        }
    }
    let startup_registry = registry_for_startup;

    iced::application(
        move || {
            let app = app.clone();
            let startup =
                if open_explicit {
                    match &startup_registry {
                        Some(registry) => {
                            let registry = registry.clone();
                            let repo_path = repo_path.clone();
                            Task::perform(
                                async move {
                                    Message::OpenRepoResult(
                                        crate::server::websocket::open_repo_tab(
                                            &registry,
                                            String::new(),
                                            repo_path.display().to_string(),
                                        )
                                        .await,
                                    )
                                },
                                |m| m,
                            )
                        }
                        None => Task::none(),
                    }
                } else {
                    Task::none()
                };
            (app, startup)
        },
        GritApp::update,
        GritApp::view,
    )
    .title("Grit")
    .theme(iced::Theme::Dark)
    .window_size(iced::Size::new(960.0, 680.0))
    .subscription(GritApp::subscription)
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::types::{FileChange, GitStatus};
    use crate::server::registry::{WebState, WebTab};

    fn webtab(id: usize, name: &str, path: &std::path::Path) -> WebTab {
        WebTab {
            id,
            name: name.to_string(),
            repo_path: path.display().to_string(),
            state: RepoState::default(),
        }
    }

    fn app_in(_dir: &std::path::Path) -> GritApp {
        GritApp::new()
    }

    /// Seeds one repo tab (id 0) through the normal sync path.
    fn seed_one(app: &mut GritApp, dir: &std::path::Path) {
        let _ = app.update(Message::WebTabsSync(vec![webtab(0, "repo", dir)]));
    }

    fn repo_state(branch: &str) -> RepoState {
        RepoState {
            current_branch: branch.to_string(),
            branches: vec![branch.to_string()],
            changes: vec![],
            history: vec![],
        }
    }

    #[test]
    fn commit_message_changed_updates_active_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::CommitMessageChanged("fix bug".to_string()));
        assert_eq!(app.active_repo().unwrap().commit_message, "fix bug");
    }

    #[test]
    fn empty_commit_message_sets_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::CommitPressed);
        let tab = app.active_repo().unwrap();
        assert!(tab.error.is_some());
        assert!(tab.commit_message.is_empty());
    }

    #[test]
    fn non_empty_commit_message_clears_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::CommitMessageChanged("fix bug".to_string()));
        let _ = app.update(Message::CommitPressed);
        assert!(app.active_repo().unwrap().commit_message.is_empty());
    }

    #[test]
    fn state_updated_replaces_repo_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let state = repo_state("main");
        let _ = app.update(Message::TabStateUpdated(0, state.clone()));
        let tab = app.active_repo().unwrap();
        assert_eq!(tab.repo_state, state);
        assert!(tab.error.is_none());
    }

    #[test]
    fn error_message_is_stored_for_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::TabError(0, "boom".to_string()));
        assert_eq!(app.active_repo().unwrap().error.as_deref(), Some("boom"));
    }

    #[test]
    fn add_tab_pressed_shows_local_form_without_creating_a_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::AddTabPressed);
        assert_eq!(app.tabs.len(), 1, "the form must never become a tab");
        assert!(app.showing_add_form());
    }

    #[test]
    fn open_tab_switches_active_and_hides_form() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let other = tempfile::tempdir().unwrap();
        let _ = app.update(Message::WebTabsSync(vec![
            webtab(0, "one", dir.path()),
            webtab(1, "two", other.path()),
        ]));
        let _ = app.update(Message::AddTabPressed);
        let _ = app.update(Message::OpenTab(0));
        assert_eq!(app.active, 0);
        assert!(!app.showing_add_form());
    }

    #[tokio::test]
    async fn open_new_repo_rejects_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::AddTabPressed);
        let _ = app.update(Message::NewRepoPathChanged(
            "/nonexistent/does-not-exist".to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        assert!(app.showing_add_form(), "form stays open on error");
        assert!(
            app.add_error.is_some(),
            "validation error must be surfaced"
        );
        assert!(app.tabs.is_empty(), "no tab may be created locally");
    }

    #[tokio::test]
    async fn open_repo_operation_appends_and_sync_adopts_and_selects() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let registry = crate::server::registry::TabRegistry::new();
        app.registry = Some(registry.clone());

        let repo_dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        let id = crate::server::websocket::open_repo_tab(
            &registry,
            "My Repo".to_string(),
            repo_dir.path().display().to_string(),
        )
        .await
        .unwrap();
        let _ = app.update(Message::OpenRepoResult(Ok(id)));
        let _ = app.update(Message::WebTabsSync(registry.snapshot().tabs));

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_repo().unwrap().name, "My Repo");
        assert_eq!(app.active_repo().unwrap().id, id);
        assert_eq!(app.active, 0, "adopted tab becomes active");
        assert!(!app.showing_add_form(), "form hides once a repo opens");
        assert!(app.add_name.is_empty() && app.add_path.is_empty(), "form resets");
    }

    #[test]
    fn close_tab_through_registry_arrives_via_sync() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let mut app = app_in(dir.path());
        let registry = crate::server::registry::TabRegistry::with_single_tab(
            3,
            "gone".to_string(),
            dir.path().to_path_buf(),
        );
        app.registry = Some(registry.clone());
        let _ = app.update(Message::WebTabsSync(registry.snapshot().tabs));
        assert_eq!(app.tabs.len(), 1);

        // The web closes tab 3; the snapshot no longer contains it.
        crate::server::websocket::close_tab_by_id(&registry, 3);
        let _ = app.update(Message::WebTabsSync(registry.snapshot().tabs));

        assert!(app.tabs.is_empty(), "closed tab must disappear locally");
        assert!(dir.path().join(".git").exists(), "disk untouched");
    }

    #[test]
    fn diff_loaded_stores_diff_on_active_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let mut state = repo_state("main");
        state.changes = vec![FileChange {
            path: "a.txt".to_string(),
            status: GitStatus::Modified,
            is_staged: false,
        }];
        let _ = app.update(Message::TabStateUpdated(0, state));
        let _ = app.update(Message::DiffLoaded(
            "a.txt".to_string(),
            "-old\n+new\n".to_string(),
        ));
        assert_eq!(app.active_repo().unwrap().diff.as_deref(), Some("-old\n+new\n"));
    }

    #[test]
    fn diff_loaded_ignores_stale_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::DiffLoaded(
            "missing.txt".to_string(),
            "diff".to_string(),
        ));
        assert!(app.active_repo().unwrap().diff.is_none());
    }

    #[test]
    fn nuke_requires_two_presses() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::NukePressed);
        assert!(app.active_repo().unwrap().nuke_armed);
        let _ = app.update(Message::NukePressed);
        assert!(!app.active_repo().unwrap().nuke_armed);
    }

    #[test]
    fn state_update_disarms_nuke() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());
        let _ = app.update(Message::NukePressed);
        assert!(app.active_repo().unwrap().nuke_armed);
        let _ = app.update(Message::TabStateUpdated(0, RepoState::default()));
        assert!(!app.active_repo().unwrap().nuke_armed);
    }

    #[test]
    fn web_removed_tab_is_dropped_locally() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let registry = crate::server::registry::TabRegistry::new();
        registry.set(WebState {
            active: 0,
            tabs: vec![webtab(0, "grit", dir.path())],
        });
        let _ = app.update(Message::WebTabsSync(registry.snapshot().tabs));
        assert_eq!(app.tabs.len(), 1);

        registry.set(WebState {
            active: 0,
            tabs: Vec::new(),
        });
        let _ = app.update(Message::WebTabsSync(registry.snapshot().tabs));

        assert!(app.tabs.is_empty());
        assert!(app.showing_add_form(), "zero tabs fall back to the form");
    }

    #[test]
    fn web_created_tab_is_adopted_by_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::WebTabsSync(vec![webtab(7, "from web", dir.path())]));
        assert!(
            app.tabs.iter().any(|t| t.id == 7),
            "desktop must adopt tabs created through the web UI"
        );
    }

    #[test]
    fn web_placeholder_tabs_are_not_adopted() {
        let mut app = GritApp::new();
        let _ = app.update(Message::WebTabsSync(vec![WebTab {
            id: 9,
            name: "new".to_string(),
            repo_path: String::new(),
            state: RepoState::default(),
        }]));
        assert!(
            !app.tabs.iter().any(|t| t.id == 9),
            "empty-path entries are not repositories"
        );
    }

    #[test]
    fn adopt_web_repo_selects_it_and_hides_the_form() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = GritApp::new();
        let _ = app.update(Message::AddTabPressed);
        assert!(app.showing_add_form());

        let _ = app.update(Message::WebTabsSync(vec![webtab(0, "fresh", dir.path())]));

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_repo().unwrap().name, "fresh");
        assert_eq!(app.active, 0);
        assert!(!app.showing_add_form(), "stale form must not linger");
    }

    #[test]
    fn dead_id_is_healed_when_the_same_id_returns() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::WebTabsSync(vec![webtab(5, "first", dir.path())]));
        let _ = app.update(Message::WebTabsSync(vec![]));
        assert!(app.tabs.is_empty());

        // A later snapshot re-uses the id for a different repo.
        let _ = app.update(Message::WebTabsSync(vec![webtab(
            5,
            "returned",
            dir.path(),
        )]));
        assert!(
            app.tabs.iter().any(|t| t.name == "returned"),
            "a re-used live id must be adoptable, not permanently dead"
        );
    }

    #[test]
    fn sync_merge_preserves_local_ui_fields_but_takes_server_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        seed_one(&mut app, dir.path());

        let _ = app.update(Message::CommitMessageChanged("wip message".to_string()));
        let _ = app.update(Message::TabError(0, "transient".to_string()));

        // The server renames the tab and delivers fresh state.
        let renamed = WebTab {
            id: 0,
            name: "renamed".to_string(),
            repo_path: dir.path().display().to_string(),
            state: repo_state("dev"),
        };
        let _ = app.update(Message::WebTabsSync(vec![renamed]));

        let tab = app.active_repo().unwrap();
        assert_eq!(tab.name, "renamed", "server owns identity fields");
        assert_eq!(tab.repo_state.current_branch, "dev");
        assert_eq!(
            tab.commit_message, "wip message",
            "local draft must survive broadcasts"
        );
        assert_eq!(tab.error.as_deref(), Some("transient"));
    }

    #[test]
    fn zero_tabs_default_to_add_form() {
        let app = GritApp::new();
        assert!(app.showing_add_form());
    }

    #[test]
    fn remote_mode_close_tab_waits_for_server_echo() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let mut app = GritApp::new();
        app.remote_port = Some(5999);
        let live = vec![WebTab {
            id: 0,
            name: "t".to_string(),
            repo_path: dir.path().display().to_string(),
            state: crate::git::types::RepoState::default(),
        }];
        app.apply_sync(&live);

        let _ = app.update(Message::CloseTab(0));
        assert_eq!(
            app.tabs.len(),
            1,
            "remote close must not mutate local tabs; the echo does"
        );
    }
}
