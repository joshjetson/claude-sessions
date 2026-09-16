//! Presentation vocabulary shared by the views.

use serde::{Deserialize, Serialize};

/// The colour names the Node app used as blessed tags. Kept as an enum so the
/// UI phase maps them to ratatui styles in exactly one place; config still
/// stores colours as free strings so an unrecognised name round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    Green,
    Yellow,
    Gray,
    Cyan,
    Magenta,
    Red,
    Blue,
    White,
}

impl Color {
    pub fn as_str(self) -> &'static str {
        match self {
            Color::Green => "green",
            Color::Yellow => "yellow",
            Color::Gray => "gray",
            Color::Cyan => "cyan",
            Color::Magenta => "magenta",
            Color::Red => "red",
            Color::Blue => "blue",
            Color::White => "white",
        }
    }
}

/// Which tab the dashboard opens on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultView {
    Sessions,
    #[default]
    Board,
    Deploy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalDriverName {
    #[default]
    Auto,
    Iterm2,
    Tmux,
}
