//! Diff panel: renders the diff of the currently selected file.

use iced::widget::{column, scrollable, text};
use iced::{Element, Length, Font};

use crate::ui::state::Message;

pub fn diff(diff: &Option<String>) -> Element<'_, Message> {
    let content = match diff {
        Some(diff) if !diff.trim().is_empty() => {
            let lines: Vec<Element<'_, Message>> = diff
                .lines()
                .map(|line| text(line).font(Font::MONOSPACE).size(12).into())
                .collect();
            column(lines).spacing(0)
        }
        _ => column![text("Click a file in the changes list to view its diff")
            .size(14)
            .color(iced::Color::from_rgb(0.5, 0.5, 0.5))],
    };

    scrollable(content)
        .height(Length::FillPortion(4))
        .width(Length::Fill)
        .into()
}