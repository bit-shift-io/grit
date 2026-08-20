//! Header panel: branch selector and push / pull actions.

use iced::widget::{button, pick_list, row, text};
use iced::{Element, Length};

use crate::git::types::RepoState;
use crate::ui::state::Message;

pub fn header(repo_state: &RepoState, nuke_armed: bool) -> Element<'_, Message> {
    let nuke_label = if nuke_armed {
        "Confirm Nuke?"
    } else {
        "Nuke"
    };
    row![
        text("Grit").size(22),
        pick_list(
            repo_state.branches.clone(),
            Some(repo_state.current_branch.clone()),
            Message::CheckoutBranch,
        ),
        button("Push").on_press(Message::PushPressed),
        button("Pull").on_press(Message::PullPressed),
        button(nuke_label).on_press(Message::NukePressed).style(nuke_style),
    ]
    .spacing(10)
    .width(Length::Fill)
    .into()
}

fn nuke_style(
    theme: &iced::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let mut style = iced::widget::button::Style::default();
    let palette = theme.palette();
    style.background = Some(iced::Background::Color(if matches!(
        status,
        iced::widget::button::Status::Hovered
    ) {
        iced::Color::from_rgb(0.75, 0.1, 0.1)
    } else {
        iced::Color::from_rgb(0.55, 0.08, 0.08)
    }));
    style.text_color = palette.background;
    style
}