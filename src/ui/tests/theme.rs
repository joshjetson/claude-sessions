//! Theme resolution — including the quirk that looks backwards until you see
//! what the settings grid writes.

use ratatui::style::Color;

use crate::config::ChatConfig;
use crate::ui::theme::{color_from_name, named_theme, resolve_theme, COLOR_NAMES, THEME_NAMES};

#[test]
fn the_four_themes_are_all_present() {
    assert_eq!(THEME_NAMES, ["default", "solarized", "monokai", "minimal"]);
    assert_eq!(named_theme("monokai").user_color, "magenta");
    assert_eq!(named_theme("minimal").user_label, ">");
    assert_eq!(named_theme("minimal").assistant_label, "<");
}

#[test]
fn an_unknown_theme_name_falls_back_to_default() {
    assert_eq!(named_theme("dracula"), named_theme("default"));
}

#[test]
fn a_config_matching_its_theme_resolves_to_that_theme() {
    // This is the state the settings grid leaves behind: cycling the theme
    // copies its six values into the config.
    let monokai = named_theme("monokai");
    let chat = ChatConfig {
        theme: "monokai".into(),
        user_color: monokai.user_color.clone(),
        assistant_color: monokai.assistant_color.clone(),
        tool_color: monokai.tool_color.clone(),
        code_color: monokai.code_color.clone(),
        user_label: monokai.user_label.clone(),
        assistant_label: monokai.assistant_label.clone(),
        ..ChatConfig::default()
    };
    assert_eq!(resolve_theme(&chat), monokai);
}

#[test]
fn a_field_that_differs_from_the_theme_wins() {
    // THE QUIRK: "differs from the base" is how Node spelled "the user chose
    // this". A default ChatConfig carries the *default* theme's colours, so
    // naming a different theme without going through the grid leaves the old
    // colours in place — reproduced here deliberately.
    let chat = ChatConfig {
        theme: "monokai".into(),
        ..ChatConfig::default()
    };
    let resolved = resolve_theme(&chat);
    assert_eq!(
        resolved.user_color, "cyan",
        "default config colour survived"
    );
    assert_eq!(
        named_theme("monokai").user_color,
        "magenta",
        "the theme itself still says magenta"
    );
}

#[test]
fn an_explicit_override_survives_resolution() {
    let chat = ChatConfig {
        theme: "default".into(),
        tool_color: "red".into(),
        ..ChatConfig::default()
    };
    assert_eq!(resolve_theme(&chat).tool_color, "red");
}

#[test]
fn colour_names_map_to_the_terminal_palette_not_to_rgb() {
    // ANSI slots so the user's own palette decides what "cyan" looks like.
    assert_eq!(color_from_name("cyan"), Color::Cyan);
    assert_eq!(color_from_name("gray"), Color::DarkGray);
    assert_eq!(color_from_name("grey"), Color::DarkGray);
    assert_eq!(color_from_name("brightred"), Color::LightRed);
}

#[test]
fn an_unknown_colour_name_resets_rather_than_guessing() {
    assert_eq!(color_from_name("chartreuse"), Color::Reset);
    assert_eq!(color_from_name(""), Color::Reset);
}

#[test]
fn hex_is_accepted_for_the_diff_backgrounds() {
    assert_eq!(color_from_name("#5a1f1f"), Color::Rgb(0x5a, 0x1f, 0x1f));
    assert_eq!(color_from_name("#zzzzzz"), Color::Reset);
    assert_eq!(color_from_name("#abc"), Color::Reset);
}

#[test]
fn every_colour_the_settings_grid_offers_resolves() {
    for name in COLOR_NAMES {
        assert_ne!(
            color_from_name(name),
            Color::Reset,
            "settings offers {name} but it does not resolve"
        );
    }
}
