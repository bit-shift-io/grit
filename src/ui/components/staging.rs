//! Staging panel: split view of unstaged and staged changes.

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};

use crate::git::types::FileChange;
use crate::ui::state::Message;

pub fn staging(changes: &[FileChange]) -> Element<'_, Message> {
    let unstaged = changes.iter().filter(|c| !c.is_staged);
    let staged = changes.iter().filter(|c| c.is_staged);

    row![
        column![
            text("Unstaged Changes").size(16),
            change_list(unstaged, false),
        ]
        .spacing(6)
        .width(Length::FillPortion(1)),
        column![
            text("Staged Changes").size(16),
            change_list(staged, true),
        ]
        .spacing(6)
        .width(Length::FillPortion(1)),
    ]
    .spacing(12)
    .height(Length::FillPortion(3))
    .width(Length::Fill)
    .into()
}

fn change_list<'a>(
    changes: impl Iterator<Item = &'a FileChange>,
    staged: bool,
) -> Element<'a, Message> {
    let rows: Vec<Element<'a, Message>> = changes.map(|c| change_row(c, staged)).collect();
    if rows.is_empty() {
        return text(if staged {
            "Nothing staged"
        } else {
            "No changes"
        })
        .into();
    }

    scrollable(column(rows).spacing(4))
        .height(Length::Fill)
        .into()
}

fn change_row(change: &FileChange, staged: bool) -> Element<'_, Message> {
    let action = if staged {
        button("Unstage").on_press(Message::UnstageFile(change.path.clone()))
    } else {
        button("Stage").on_press(Message::StageFile(change.path.clone()))
    };

    row![
        text(format!("{:?}", change.status)).width(Length::Fixed(90.0)),
        text(&change.path),
        action,
    ]
    .spacing(8)
    .into()
}