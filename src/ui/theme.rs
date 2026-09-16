//! The four conversation themes, and the one place colour names become styles.
//!
//! Ported from the Node app's `src/themes.js`. Colours stay *names* in config so
//! a hand-edited file round-trips an unrecognised value, and become
//! [`ratatui::style::Color`] only here — the 16 ANSI slots rather than RGB, so a
//! user's terminal palette still decides what "cyan" looks like.

use ratatui::style::Color;

use crate::config::ChatConfig;

/// Order matters: the settings grid cycles through this list.
pub const THEME_NAMES: [&str; 4] = ["default", "solarized", "monokai", "minimal"];

/// The colour names the settings grid offers. Config may hold anything; this is
/// only what the `←→` cycle walks.
pub const COLOR_NAMES: [&str; 8] = [
    "cyan", "green", "yellow", "magenta", "red", "blue", "white", "gray",
];

/// A theme as `themes.js` stored it: six fields, colours still as names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub user_color: String,
    pub assistant_color: String,
    pub tool_color: String,
    pub code_color: String,
    pub user_label: String,
    pub assistant_label: String,
}

impl Theme {
    fn of(
        user: &str,
        assistant: &str,
        tool: &str,
        code: &str,
        user_label: &str,
        assistant_label: &str,
    ) -> Self {
        Theme {
            user_color: user.into(),
            assistant_color: assistant.into(),
            tool_color: tool.into(),
            code_color: code.into(),
            user_label: user_label.into(),
            assistant_label: assistant_label.into(),
        }
    }

    pub fn user(&self) -> Color {
        color_from_name(&self.user_color)
    }

    pub fn assistant(&self) -> Color {
        color_from_name(&self.assistant_color)
    }

    pub fn tool(&self) -> Color {
        color_from_name(&self.tool_color)
    }

    pub fn code(&self) -> Color {
        color_from_name(&self.code_color)
    }
}

impl Default for Theme {
    fn default() -> Self {
        named_theme("default")
    }
}

/// One of the four built-ins; anything unrecognised is `default`, as in Node.
pub fn named_theme(name: &str) -> Theme {
    match name {
        "solarized" => Theme::of("blue", "green", "yellow", "cyan", "You", "Claude"),
        "monokai" => Theme::of("magenta", "green", "yellow", "red", "You", "Claude"),
        "minimal" => Theme::of("white", "white", "gray", "gray", ">", "<"),
        _ => Theme::of("cyan", "green", "yellow", "magenta", "You", "Claude"),
    }
}

/// Merge the chat config's per-field colours over its chosen theme.
///
/// The quirk, ported exactly: a field from config wins **only when it differs
/// from the selected theme's own value**. That reads backwards until you see
/// the settings grid — cycling the theme there writes the theme's six values
/// into the config, so "differs from the base" is how Node distinguished a
/// value the user actually chose from the theme's own default sitting in the
/// file. Change the rule and every saved config silently overrides its theme.
pub fn resolve_theme(chat: &ChatConfig) -> Theme {
    let base = named_theme(&chat.theme);
    let pick = |configured: &str, base: &str| -> String {
        if configured != base {
            configured.to_string()
        } else {
            base.to_string()
        }
    };
    Theme {
        user_color: pick(&chat.user_color, &base.user_color),
        assistant_color: pick(&chat.assistant_color, &base.assistant_color),
        tool_color: pick(&chat.tool_color, &base.tool_color),
        code_color: pick(&chat.code_color, &base.code_color),
        user_label: pick(&chat.user_label, &base.user_label),
        assistant_label: pick(&chat.assistant_label, &base.assistant_label),
    }
}

/// A blessed-era colour name as a ratatui colour.
///
/// The ANSI slots are deliberate: `Color::Cyan` asks the terminal for its own
/// cyan, so the dashboard sits inside the user's theme instead of fighting it.
/// `gray` maps to `DarkGray` (ANSI bright-black) because that is what blessed's
/// `{gray-fg}` rendered as. `#rrggbb` is accepted for the two diff backgrounds,
/// which have to be specific shades to read as a diff at all.
pub fn color_from_name(name: &str) -> Color {
    if let Some(rgb) = parse_hex(name) {
        return rgb;
    }
    match name {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::DarkGray,
        "brightred" => Color::LightRed,
        "brightgreen" => Color::LightGreen,
        "brightyellow" => Color::LightYellow,
        "brightblue" => Color::LightBlue,
        "brightmagenta" => Color::LightMagenta,
        "brightcyan" => Color::LightCyan,
        // An unknown name is the terminal's default foreground rather than a
        // guess: a typo in config should not repaint the pane.
        _ => Color::Reset,
    }
}

/// The crate's own [`crate::types::Color`] vocabulary, so status dots and
/// activity ages resolve through the same table as everything else.
pub fn color_of(color: crate::types::Color) -> Color {
    color_from_name(color.as_str())
}

fn parse_hex(name: &str) -> Option<Color> {
    let hex = name.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}
