//! Iced application state, message dispatch, and background-task wiring.

use std::path::PathBuf;

use iced::widget::{button, column, row, rule, text, text_input};
use iced::{Element, Length, Subscription, Task};

use crate::git::types::{GitAction, RepoState};
use crate::ui::components;
use crate::ui::persistence::{self, SavedRepo};

/// UI events produced by widgets and background tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    // Tab management.
    AddTabPressed,
    OpenTab(usize),
    CloseTab(usize),
    NewRepoNameChanged(String),
    NewRepoPathChanged(String),
    BrowseFolder,
    FolderPicked(Option<PathBuf>),
    OpenNewRepo,
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

/// One tab: either a live repository or the "add repository" form.
#[derive(Debug, Clone)]
pub enum Tab {
    Repo(RepoTab),
    AddRepo {
        name: String,
        path: String,
        error: Option<String>,
    },
}

/// Root application state for the native desktop UI.
#[derive(Debug, Clone)]
pub struct GritApp {
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    config_path: Option<PathBuf>,
    registry: Option<crate::server::registry::TabRegistry>,
}

impl GritApp {
    /// Creates an app whose persistence target is overridden (used by tests).
    #[cfg(test)]
    fn with_config(repo_path: PathBuf, config_path: Option<PathBuf>) -> Self {
        let tab = RepoTab {
            id: 0,
            name: Self::tab_name(&repo_path),
            repo_path,
            repo_state: RepoState::default(),
            commit_message: String::new(),
            diff: None,
            nuke_armed: false,
            error: None,
        };
        Self {
            tabs: vec![Tab::Repo(tab)],
            active: 0,
            next_id: 1,
            config_path,
            registry: None,
        }
    }

    /// Builds the app from previously saved repositories, falling back to a CLI path.
    fn from_saved(saved: Vec<SavedRepo>, fallback: PathBuf) -> Self {
        let mut tabs = Vec::new();
        for (id, repo) in saved.into_iter().enumerate() {
            tabs.push(Tab::Repo(RepoTab {
                id,
                name: repo.name,
                repo_path: repo.path,
                repo_state: RepoState::default(),
                commit_message: String::new(),
                diff: None,
                nuke_armed: false,
                error: None,
            }));
        }
        let next_id = tabs.len();
        if tabs.is_empty() {
            tabs.push(Tab::Repo(RepoTab {
                id: 0,
                name: Self::tab_name(&fallback),
                repo_path: fallback,
                repo_state: RepoState::default(),
                commit_message: String::new(),
                diff: None,
                nuke_armed: false,
                error: None,
            }));
        }
        Self {
            tabs,
            active: 0,
            next_id: if next_id == 0 { 1 } else { next_id },
            config_path: persistence::config_path(),
            registry: None,
        }
    }

    fn tab_name(path: &PathBuf) -> String {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }

    fn active_repo(&self) -> Option<&RepoTab> {
        match self.tabs.get(self.active) {
            Some(Tab::Repo(tab)) => Some(tab),
            _ => None,
        }
    }

    fn active_repo_mut(&mut self) -> Option<&mut RepoTab> {
        match self.tabs.get_mut(self.active) {
            Some(Tab::Repo(tab)) => Some(tab),
            _ => None,
        }
    }

    /// Persists the current repository tabs to the data folder.
    fn persist_tabs(&self) {
        if let Some(path) = &self.config_path {
            let repos: Vec<SavedRepo> = self
                .tabs
                .iter()
                .filter_map(|t| match t {
                    Tab::Repo(r) => Some(SavedRepo {
                        name: r.name.clone(),
                        path: r.repo_path.clone(),
                    }),
                    _ => None,
                })
                .collect();
            persistence::save_repos(path, &repos);
        }
    }

    /// Publishes the current repo tabs + active index to the web registry,
    /// preserving tabs that were created through the web UI. The published
    /// `active` is only an advisory default for newly connected web clients;
    /// each client owns its tab selection.
    fn sync_registry(&self) {
        if let Some(registry) = &self.registry {
            use crate::server::registry::{WebState, WebTab};
            let snapshot = registry.snapshot();
            let mut tabs: Vec<WebTab> = self
                .tabs
                .iter()
                .filter_map(|t| match t {
                    Tab::Repo(r) => Some(WebTab {
                        id: r.id,
                        name: r.name.clone(),
                        repo_path: r.repo_path.display().to_string(),
                        state: r.repo_state.clone(),
                    }),
                    _ => None,
                })
                .collect();
            let gui_ids: Vec<usize> = tabs.iter().map(|t| t.id).collect();
            tabs.extend(
                snapshot
                    .tabs
                    .into_iter()
                    .filter(|t| !gui_ids.contains(&t.id)),
            );
            let len = tabs.len();
            let active = self
                .active_repo()
                .and_then(|repo| tabs.iter().position(|t| t.id == repo.id))
                .unwrap_or_else(|| snapshot.active.min(len.saturating_sub(1)));
            registry.set(WebState { active, tabs });
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AddTabPressed => {
                self.tabs.push(Tab::AddRepo {
                    name: String::new(),
                    path: String::new(),
                    error: None,
                });
                self.active = self.tabs.len() - 1;
                Task::none()
            }
            Message::OpenTab(index) => {
                if index < self.tabs.len() {
                    self.active = index;
                    self.sync_registry();
                }
                Task::none()
            }
            Message::CloseTab(index) => {
                if index >= self.tabs.len() {
                    return Task::none();
                }
                let removed_repo = match self.tabs.get(index) {
                    Some(Tab::Repo(_)) => true,
                    _ => false,
                };
                self.tabs.remove(index);
                if self.tabs.is_empty() {
                    self.tabs.push(Tab::AddRepo {
                        name: String::new(),
                        path: String::new(),
                        error: None,
                    });
                }
                if self.active >= self.tabs.len() {
                    self.active = self.tabs.len() - 1;
                } else if index < self.active {
                    self.active -= 1;
                }
                if removed_repo {
                    self.persist_tabs();
                    self.sync_registry();
                }
                Task::none()
            }
            Message::NewRepoNameChanged(name) => {
                if let Some(Tab::AddRepo { name: n, .. }) = self.tabs.get_mut(self.active) {
                    *n = name;
                }
                Task::none()
            }
            Message::NewRepoPathChanged(path) => {
                if let Some(Tab::AddRepo { path: p, .. }) = self.tabs.get_mut(self.active) {
                    *p = path;
                }
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
                    if let Some(Tab::AddRepo { path: input, .. }) = self.tabs.get_mut(self.active)
                    {
                        *input = path.display().to_string();
                    }
                }
                Task::none()
            }
            Message::OpenNewRepo => {
                let (name, path) = match self.tabs.get(self.active) {
                    Some(Tab::AddRepo { name, path, .. }) => (name.clone(), path.clone()),
                    _ => return Task::none(),
                };
                let name = name.trim().to_string();
                let path = path.trim().to_string();
                let dir = PathBuf::from(&path);
                let validation_error = if path.is_empty() {
                    Some("Folder path is required".to_string())
                } else if !dir.is_dir() {
                    Some(format!("Not a directory: {path}"))
                } else {
                    None
                };
                if let Some(error) = validation_error {
                    if let Some(Tab::AddRepo { error: slot, .. }) = self.tabs.get_mut(self.active)
                    {
                        *slot = Some(error);
                    }
                    return Task::none();
                }

                let tab_name = if name.is_empty() {
                    Self::tab_name(&dir)
                } else {
                    name
                };
                let id = self.next_id;
                self.next_id += 1;
                if let Some(slot) = self.tabs.get_mut(self.active) {
                    *slot = Tab::Repo(RepoTab {
                        id,
                        name: tab_name,
                        repo_path: dir.clone(),
                        repo_state: RepoState::default(),
                        commit_message: String::new(),
                        diff: None,
                        nuke_armed: false,
                        error: None,
                    });
                }
                self.persist_tabs();
                self.sync_registry();
                refresh(id, dir)
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
                let path = self.tabs.iter().find_map(|t| match t {
                    Tab::Repo(r) if r.id == id => Some(r.repo_path.clone()),
                    _ => None,
                });
                match path {
                    Some(path) => refresh(id, path),
                    None => Task::none(),
                }
            }
            Message::TabStateUpdated(id, state) => {
                if let Some(Tab::Repo(tab)) = self
                    .tabs
                    .iter_mut()
                    .find(|t| matches!(t, Tab::Repo(r) if r.id == id))
                {
                    tab.repo_state = state;
                    tab.nuke_armed = false;
                    tab.error = None;
                }
                self.sync_registry();
                Task::none()
            }
            Message::TabError(id, error) => {
                if let Some(Tab::Repo(tab)) = self
                    .tabs
                    .iter_mut()
                    .find(|t| matches!(t, Tab::Repo(r) if r.id == id))
                {
                    tab.error = Some(error);
                }
                Task::none()
            }
        }
    }

    /// Runs a git action against the active tab on an executor worker thread.
    fn run_action(&self, action: GitAction) -> Task<Message> {
        let Some(tab) = self.active_repo() else {
            return Task::none();
        };
        run_action_on(tab.id, tab.repo_path.clone(), action)
    }

    /// Watches every open repository and emits per-tab refresh events.
    pub fn subscription(&self) -> Subscription<Message> {
        let subs: Vec<Subscription<Message>> = self
            .tabs
            .iter()
            .filter_map(|t| match t {
                Tab::Repo(tab) => Some(watcher_subscription(tab.id, tab.repo_path.clone())),
                _ => None,
            })
            .collect();
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
                let label = match tab {
                    Tab::Repo(t) => t.name.clone(),
                    Tab::AddRepo { .. } => "New Repo".to_string(),
                };
                let is_active = index == self.active;
                let tab_button = button(text(label))
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
        match self.tabs.get(self.active) {
            Some(Tab::Repo(tab)) => self.repo_view(tab),
            Some(Tab::AddRepo { name, path, error }) => {
                add_repo_view(name, path, error.as_ref(), self.active)
            }
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

/// Form for creating a new repository tab.
fn add_repo_view<'a>(
    name: &'a str,
    path: &'a str,
    error: Option<&'a String>,
    active: usize,
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
            button("Cancel").on_press(Message::CloseTab(active)),
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

/// Launches the native GUI, restoring saved repositories from the data folder.
pub fn run(registry: crate::server::registry::TabRegistry, repo_path: PathBuf) -> iced::Result {
    let mut app = GritApp::from_saved(persistence::load_repos(), repo_path);
    app.registry = Some(registry);
    app.sync_registry();
    app.persist_tabs();
    iced::application(
        move || {
            let app = app.clone();
            let tasks: Vec<Task<Message>> = app
                .tabs
                .iter()
                .filter_map(|t| match t {
                    Tab::Repo(tab) => Some(refresh(tab.id, tab.repo_path.clone())),
                    _ => None,
                })
                .collect();
            (app, Task::batch(tasks))
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

    fn app_in(dir: &std::path::Path) -> GritApp {
        GritApp::with_config(PathBuf::from("."), Some(dir.join("repos.json")))
    }

    #[test]
    fn commit_message_changed_updates_active_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::CommitMessageChanged("fix bug".to_string()));
        assert_eq!(app.active_repo().unwrap().commit_message, "fix bug");
    }

    #[test]
    fn empty_commit_message_sets_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::CommitPressed);
        let tab = app.active_repo().unwrap();
        assert!(tab.error.is_some());
        assert!(tab.commit_message.is_empty());
    }

    #[test]
    fn non_empty_commit_message_clears_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::CommitMessageChanged("fix bug".to_string()));
        let _ = app.update(Message::CommitPressed);
        assert!(app.active_repo().unwrap().commit_message.is_empty());
    }

    #[test]
    fn state_updated_replaces_repo_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let state = RepoState {
            current_branch: "main".to_string(),
            branches: vec!["main".to_string()],
            changes: vec![],
            history: vec![],
        };
        let _ = app.update(Message::TabStateUpdated(0, state.clone()));
        let tab = app.active_repo().unwrap();
        assert_eq!(tab.repo_state, state);
        assert!(tab.error.is_none());
    }

    #[test]
    fn error_message_is_stored_for_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::TabError(0, "boom".to_string()));
        assert_eq!(app.active_repo().unwrap().error.as_deref(), Some("boom"));
    }

    #[test]
    fn add_tab_pressed_appends_add_repo_form() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::AddTabPressed);
        assert_eq!(app.tabs.len(), 2);
        assert!(matches!(&app.tabs[app.active], Tab::AddRepo { .. }));
    }

    #[test]
    fn open_tab_switches_active() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::AddTabPressed);
        assert_eq!(app.active, 1);
        let _ = app.update(Message::OpenTab(0));
        assert_eq!(app.active, 0);
    }

    #[test]
    fn open_new_repo_converts_form_to_repo_tab_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::AddTabPressed);
        let repo_dir = tempfile::tempdir().unwrap();
        let _ = app.update(Message::NewRepoNameChanged("My Repo".to_string()));
        let _ = app.update(Message::NewRepoPathChanged(
            repo_dir.path().display().to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        let tab = app.active_repo().unwrap();
        assert_eq!(tab.name, "My Repo");
        assert_eq!(tab.repo_path, repo_dir.path());

        let saved = persistence::load_repos_from(&dir.path().join("repos.json"));
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|r| r.name == "My Repo"));
    }

    #[test]
    fn open_new_repo_uses_dir_name_when_tab_unnamed() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::AddTabPressed);
        let repo_dir = tempfile::tempdir().unwrap();
        let _ = app.update(Message::NewRepoPathChanged(
            repo_dir.path().display().to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        let tab = app.active_repo().unwrap();
        assert_eq!(
            tab.name,
            GritApp::tab_name(&repo_dir.path().to_path_buf())
        );
    }

    #[test]
    fn open_new_repo_rejects_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::AddTabPressed);
        let _ = app.update(Message::NewRepoPathChanged(
            "/nonexistent/does-not-exist".to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        assert!(matches!(&app.tabs[app.active], Tab::AddRepo { .. }));
        let Tab::AddRepo { error, .. } = &app.tabs[app.active] else {
            panic!("expected add repo form");
        };
        assert!(error.is_some());
    }

    #[test]
    fn close_repo_tab_removes_it_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::CloseTab(0));
        assert_eq!(app.tabs.len(), 1);
        assert!(matches!(&app.tabs[0], Tab::AddRepo { .. }));
        assert!(persistence::load_repos_from(&dir.path().join("repos.json")).is_empty());
    }

    #[test]
    fn from_saved_restores_multiple_tabs() {
        let _dir = tempfile::tempdir().unwrap();
        let saved = vec![
            SavedRepo {
                name: "alpha".to_string(),
                path: PathBuf::from("/repo/alpha"),
            },
            SavedRepo {
                name: "beta".to_string(),
                path: PathBuf::from("/repo/beta"),
            },
        ];
        let app = GritApp::from_saved(saved, PathBuf::from("/repo/fallback"));
        assert_eq!(app.tabs.len(), 2);
        assert!(matches!(app.tabs[0], Tab::Repo(_)));
        assert!(matches!(app.tabs[1], Tab::Repo(_)));
        assert_eq!(app.active, 0);
    }

    #[test]
    fn from_saved_empty_uses_fallback() {
        let app = GritApp::from_saved(Vec::new(), PathBuf::from("/repo/fallback"));
        assert_eq!(app.tabs.len(), 1);
        let tab = app.active_repo().unwrap();
        assert_eq!(tab.repo_path, PathBuf::from("/repo/fallback"));
    }

    #[test]
    fn diff_loaded_stores_diff_on_active_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let state = RepoState {
            current_branch: "main".to_string(),
            branches: vec!["main".to_string()],
            changes: vec![FileChange {
                path: "a.txt".to_string(),
                status: GitStatus::Modified,
                is_staged: false,
            }],
            history: vec![],
        };
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
        let _ = app.update(Message::NukePressed);
        assert!(app.active_repo().unwrap().nuke_armed);
        let _ = app.update(Message::NukePressed);
        assert!(!app.active_repo().unwrap().nuke_armed);
    }

    #[test]
    fn state_update_disarms_nuke() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_in(dir.path());
        let _ = app.update(Message::NukePressed);
        assert!(app.active_repo().unwrap().nuke_armed);
        let _ = app.update(Message::TabStateUpdated(0, RepoState::default()));
        assert!(!app.active_repo().unwrap().nuke_armed);
    }

    fn app_with_registry(dir: &std::path::Path) -> (GritApp, crate::server::registry::TabRegistry)
    {
        let registry = crate::server::registry::TabRegistry::new();
        let mut app = GritApp::with_config(PathBuf::from("."), Some(dir.join("repos.json")));
        app.registry = Some(registry.clone());
        (app, registry)
    }

    #[test]
    fn registry_receives_initial_tab_on_sync() {
        let dir = tempfile::tempdir().unwrap();
        let (app, registry) = app_with_registry(dir.path());
        app.sync_registry();
        let state = registry.snapshot();
        assert_eq!(state.active, 0);
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(state.tabs[0].repo_path, ".");
    }

    #[test]
    fn registry_reflects_state_updates() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, registry) = app_with_registry(dir.path());
        let state = RepoState {
            current_branch: "dev".to_string(),
            branches: vec!["dev".to_string()],
            changes: vec![],
            history: vec![],
        };
        let _ = app.update(Message::TabStateUpdated(0, state.clone()));
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.tabs[0].state, state);
    }

    #[test]
    fn registry_updates_on_open_new_repo() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, registry) = app_with_registry(dir.path());
        let _ = app.update(Message::AddTabPressed);
        let repo_dir = tempfile::tempdir().unwrap();
        let _ = app.update(Message::NewRepoNameChanged("new repo".to_string()));
        let _ = app.update(Message::NewRepoPathChanged(
            repo_dir.path().display().to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.active, 1);
        assert!(snapshot.tabs.iter().any(|t| t.name == "new repo"));
    }

    #[test]
    fn registry_updates_on_tab_switch() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, registry) = app_with_registry(dir.path());
        let _ = app.update(Message::AddTabPressed);
        let repo_dir = tempfile::tempdir().unwrap();
        let _ = app.update(Message::NewRepoPathChanged(
            repo_dir.path().display().to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        let _ = app.update(Message::OpenTab(0));
        assert_eq!(registry.snapshot().active, 0);
    }

    #[test]
    fn sync_registry_preserves_web_only_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, registry) = app_with_registry(dir.path());
        use crate::server::registry::{WebState, WebTab};
        registry.set(WebState {
            active: 1,
            tabs: vec![
                WebTab {
                    id: 0,
                    name: "gui".to_string(),
                    repo_path: ".".to_string(),
                    state: RepoState::default(),
                },
                WebTab {
                    id: 99,
                    name: "new".to_string(),
                    repo_path: String::new(),
                    state: RepoState::default(),
                },
            ],
        });
        let _ = app.update(Message::TabStateUpdated(0, RepoState::default()));
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.tabs.len(), 2, "web-only tab must survive gui sync");
        assert_eq!(snapshot.tabs[1].id, 99);
        assert_eq!(snapshot.active, 0, "gui selection is advisory default");
    }

    #[test]
    fn sync_registry_follows_gui_when_active_is_gui_tab() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, registry) = app_with_registry(dir.path());
        use crate::server::registry::{WebState, WebTab};
        registry.set(WebState {
            active: 0,
            tabs: vec![
                WebTab {
                    id: 0,
                    name: "gui".to_string(),
                    repo_path: ".".to_string(),
                    state: RepoState::default(),
                },
                WebTab {
                    id: 42,
                    name: "web repo".to_string(),
                    repo_path: "/elsewhere".to_string(),
                    state: RepoState::default(),
                },
            ],
        });
        let repo_dir = tempfile::tempdir().unwrap();
        let _ = app.update(Message::AddTabPressed);
        let _ = app.update(Message::NewRepoPathChanged(
            repo_dir.path().display().to_string(),
        ));
        let _ = app.update(Message::OpenNewRepo);
        let _ = app.update(Message::TabStateUpdated(1, RepoState::default()));
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.tabs.len(), 3, "web-only tab must survive gui sync");
        assert_eq!(snapshot.active, 1, "gui selection should win");
    }
}