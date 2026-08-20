//! History panel: scrollable list of recent commits.

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};

use crate::git::types::CommitInfo;
use crate::ui::state::Message;

pub fn history(history: &[CommitInfo]) -> Element<'_, Message> {
    if history.is_empty() {
        return text("No commits yet").into();
    }

    scrollable(
        column(
            history
                .iter()
                .map(|commit| {
                    row![
                        text(&commit.hash[..commit.hash.len().min(8)]),
                        text(&commit.author).width(Length::FillPortion(1)),
                        text(&commit.message),
                        button("Revert").on_press(Message::Revert(commit.hash.clone())),
                    ]
                    .spacing(8)
                    .into()
                })
                .collect::<Vec<_>>(),
        )
        .spacing(6),
    )
    .height(Length::Fill)
    .into()
}