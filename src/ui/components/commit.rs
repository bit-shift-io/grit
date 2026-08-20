//! Commit panel: message input and commit action button.

use iced::widget::{button, row, text_input};
use iced::{Element, Length};

use crate::ui::state::Message;

pub fn commit(commit_message: &str) -> Element<'_, Message> {
    row![
        text_input("Commit message", commit_message)
            .on_input(Message::CommitMessageChanged)
            .on_submit(Message::CommitPressed)
            .width(Length::Fill),
        button("Commit").on_press(Message::CommitPressed),
    ]
    .spacing(8)
    .width(Length::Fill)
    .into()
}