//! Styled row segments.
//!
//! The Node app returned rows as strings carrying blessed markup
//! (`{yellow-fg}★{/yellow-fg}`) which the renderer then parsed back out — three
//! string passes per row per frame, for every row whether visible or not. Here a
//! row is a list of `(text, style)` pairs: the UI phase maps each style to a
//! ratatui `Span` once, and the tests assert on the text and the styles rather
//! than on escape sequences.

use crate::types::Color;

/// What a segment *means*. The theme decides what that looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Ordinary row text.
    Plain,
    /// Secondary detail: counts, timestamps, stage names on subtask rows.
    Dim,
    /// Structure: expanders, stage names, the archive badge.
    Accent,
    /// A task id.
    Id,
    /// Finished, running, healthy.
    Ok,
    /// Ready to act on — a merge request that can be merged now.
    Ready,
    /// Needs attention but is not broken: drafts, blocked-on-info, priority.
    Warn,
    /// Broken or overdue.
    Danger,
    /// Informational badges.
    Info,
    /// A project name.
    Project,
}

impl Role {
    /// The colour names the Node app used, for the styles that came from data
    /// rather than from the row code (the auto-dev markers).
    pub fn from_color(color: Color) -> Role {
        match color {
            Color::Green => Role::Ok,
            Color::Yellow => Role::Warn,
            Color::Red => Role::Danger,
            Color::Cyan => Role::Accent,
            Color::Blue => Role::Info,
            Color::Magenta => Role::Project,
            Color::Gray => Role::Dim,
            Color::White => Role::Plain,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub role: Role,
    pub bold: bool,
    /// Foreground and background swapped — the flash an unread notification
    /// does on the blink tick.
    pub invert: bool,
}

impl Style {
    pub const fn new(role: Role) -> Self {
        Style {
            role,
            bold: false,
            invert: false,
        }
    }

    pub const fn plain() -> Self {
        Style::new(Role::Plain)
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub const fn invert(mut self) -> Self {
        self.invert = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub style: Style,
}

/// A formatted row: segments in display order.
pub type Row = Vec<Segment>;

/// The row as the user reads it, with the styling dropped. Tests assert on
/// this; so does anything that needs a width.
pub fn plain_text(row: &[Segment]) -> String {
    row.iter().map(|segment| segment.text.as_str()).collect()
}

/// Accumulates a row. Empty pushes are dropped so a conditional badge costs
/// nothing downstream.
#[derive(Debug, Default)]
pub struct RowBuilder {
    row: Row,
}

impl RowBuilder {
    pub fn new() -> Self {
        RowBuilder::default()
    }

    /// Unstyled text — spacing, separators, glyphs with no colour of their own.
    pub fn plain(&mut self, text: impl Into<String>) -> &mut Self {
        self.push(text, Style::plain())
    }

    pub fn styled(&mut self, text: impl Into<String>, role: Role) -> &mut Self {
        self.push(text, Style::new(role))
    }

    pub fn push(&mut self, text: impl Into<String>, style: Style) -> &mut Self {
        let text = text.into();
        if !text.is_empty() {
            self.row.push(Segment { text, style });
        }
        self
    }

    /// Append another row's segments — badges are built as small rows.
    pub fn extend(&mut self, other: Row) -> &mut Self {
        self.row
            .extend(other.into_iter().filter(|s| !s.text.is_empty()));
        self
    }

    pub fn build(self) -> Row {
        self.row
    }
}
