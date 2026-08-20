//! Header panel: branch selector and push / pull actions.

use iced::widget::{button, pick_list, row, text};
use iced::{Element, Length};

use crate::git::types::RepoState;
use crate::ui::state::Message;

pub fn header(repo_state: &RepoState) -> Element<'_, Message> {
    row![
        text("Grit").size(22),
        pick_list(
            repo_state.branches.clone(),
            Some(repo_state.current_branch.clone()),
            Message::CheckoutBranch,
        ),
        button("Push").on_press(Message::PushPressed),
        button("Pull").on_press(Message::PullPressed),
    ]
    .spacing(10)
    .width(Length::Fill)
    .into()
}