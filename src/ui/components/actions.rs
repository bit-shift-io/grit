//! Actions panel: launch discovered repository scripts (fire-and-forget).
//!
//! Picking a script from the dropdown launches it immediately; output goes
//! wherever Grit itself is running.

use iced::widget::{column, pick_list, row, text};
use iced::{Element, Length};

use crate::git::types::ScriptEntry;
use crate::ui::state::Message;

/// Renders the Actions section. Collapses to nothing when no scripts are
/// discovered.
pub fn actions(scripts: &[ScriptEntry]) -> Element<'_, Message> {
    if scripts.is_empty() {
        return column!().into();
    }

    row![
        text("Actions").size(16),
        pick_list(
            scripts.to_vec(),
            None::<ScriptEntry>,
            Message::RunScriptSelected
        )
    ]
    .spacing(10)
    .width(Length::Fill)
    .into()
}
