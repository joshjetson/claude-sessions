//! Syntax highlighting for fenced code and `Edit` diff bodies.
//!
//! Ported from the Node app's `src/syntax.js`: one pass, no language detection,
//! one keyword set spanning JS/TS, Python, Go, Rust and shell. Getting the
//! language wrong colours a word oddly; getting it "right" would mean a parser
//! per language for three lines of a diff.
//!
//! Two deliberate departures. Node ran a single alternation regex; this is a
//! hand-written scanner, because the alternatives are ordered and prefix-anchored
//! anyway and a scanner needs no regex crate. And `highlightLineAnsi` is not
//! ported at all — brief §10 lists it as dead code, written for the blessed
//! `{escape}` blocks that no longer exist.

use ratatui::style::{Color, Style};
use ratatui::text::Span;

const KEYWORDS: &[&str] = &[
    // JS/TS
    "if",
    "else",
    "while",
    "for",
    "return",
    "const",
    "let",
    "var",
    "function",
    "class",
    "import",
    "export",
    "from",
    "default",
    "new",
    "this",
    "typeof",
    "instanceof",
    "throw",
    "try",
    "catch",
    "finally",
    "switch",
    "case",
    "break",
    "continue",
    "async",
    "await",
    "yield",
    "of",
    "in",
    "do",
    "extends",
    "super",
    "static",
    "get",
    "set",
    "delete",
    "void",
    "with",
    "enum",
    "implements",
    "interface",
    "type",
    "as",
    "is",
    "keyof",
    "readonly",
    "declare",
    "module",
    "namespace",
    "abstract",
    "override", // Python
    "def",
    "elif",
    "except",
    "lambda",
    "pass",
    "raise",
    "and",
    "or",
    "not",
    "self",
    "nonlocal",
    "global",
    "assert",
    "del",
    "print", // Go
    "func",
    "package",
    "range",
    "defer",
    "go",
    "chan",
    "select",
    "map",
    "struct",
    "fallthrough",
    "goto", // Rust
    "fn",
    "mut",
    "pub",
    "mod",
    "use",
    "crate",
    "impl",
    "trait",
    "where",
    "match",
    "loop",
    "move",
    "ref",
    "unsafe",
    "extern", // Shell
    "then",
    "fi",
    "done",
    "esac",
];

const BUILTINS: &[&str] = &[
    "true",
    "false",
    "null",
    "undefined",
    "NaN",
    "Infinity",
    "None",
    "True",
    "False",
    "nil",
    "self",
    "Self",
    "console",
    "require",
    "process",
    "module",
    "exports",
    "int",
    "float",
    "str",
    "bool",
    "list",
    "dict",
    "tuple",
    "set",
    "string",
    "number",
    "boolean",
    "object",
    "any",
    "never",
    "unknown",
];

const COMMENT: Color = Color::DarkGray;
const STRING: Color = Color::Green;
const NUMBER: Color = Color::Yellow;
const DECORATOR: Color = Color::Cyan;
const KEYWORD: Color = Color::Magenta;

/// Highlight one line of code into styled spans.
///
/// `base` is the style the untokenised text keeps, so a diff body can carry the
/// added/removed background through the highlighting instead of punching holes
/// in it.
pub fn highlight_line(line: &str, base: Style) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    let mut plain_from = 0usize;

    let flush = |out: &mut Vec<Span<'static>>, from: usize, to: usize, chars: &[char]| {
        if to > from {
            out.push(Span::styled(
                chars[from..to].iter().collect::<String>(),
                base,
            ));
        }
    };

    while i < chars.len() {
        let start = i;
        match scan_token(&chars, &mut i) {
            // Not a token at all: ordinary text, taken one character at a time.
            Token::None => i = start + 1,
            // Consumed, but drawn as ordinary text — an identifier that is
            // neither keyword nor builtin. It must still be consumed as a unit,
            // or `foo` would be re-scanned from `o` and match nothing forever.
            Token::Plain => {}
            Token::Coloured(colour) => {
                flush(&mut out, plain_from, start, &chars);
                let text: String = chars[start..i].iter().collect();
                out.push(Span::styled(text, base.fg(colour)));
                plain_from = i;
            }
        }
    }
    flush(&mut out, plain_from, chars.len(), &chars);
    out
}

/// What one attempt at `*i` found.
enum Token {
    /// No token shape matched; the caller advances a single character.
    None,
    /// A token was consumed but is drawn in the base style.
    Plain,
    Coloured(Color),
}

/// Try every token shape at `*i`, in the order the Node alternation had them.
/// Advances `*i` past whatever it consumed.
fn scan_token(chars: &[char], i: &mut usize) -> Token {
    let at = *i;
    let ch = chars[at];
    let next = chars.get(at + 1).copied();

    // Comments — line, block (single-line only, as in Node), and hash.
    if ch == '/' && next == Some('/') {
        *i = chars.len();
        return Token::Coloured(COMMENT);
    }
    if ch == '/' && next == Some('*') {
        let mut j = at + 2;
        while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
            j += 1;
        }
        if j + 1 < chars.len() {
            *i = j + 2;
            return Token::Coloured(COMMENT);
        }
        return Token::None;
    }
    if ch == '#' {
        *i = chars.len();
        return Token::Coloured(COMMENT);
    }

    // Strings: template, double, single. Backslash escapes the next character.
    if ch == '`' || ch == '"' || ch == '\'' {
        let mut j = at + 1;
        while j < chars.len() {
            if chars[j] == '\\' {
                j += 2;
                continue;
            }
            if chars[j] == ch {
                *i = j + 1;
                return Token::Coloured(STRING);
            }
            j += 1;
        }
        // Unterminated: not a string token, exactly as the regex would fail.
        return Token::None;
    }

    // Numbers: hex first, then decimal with an optional fraction. The fraction
    // is only taken when a digit follows the dot — Node's trailing `\b` made
    // `1.` match as the bare integer `1`.
    if ch.is_ascii_digit() {
        if ch == '0' && matches!(next, Some('x') | Some('X')) && at + 2 < chars.len() {
            let mut j = at + 2;
            while j < chars.len() && chars[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > at + 2 {
                *i = j;
                return Token::Coloured(NUMBER);
            }
        }
        let mut j = at;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if chars.get(j) == Some(&'.') && chars.get(j + 1).is_some_and(|c| c.is_ascii_digit()) {
            j += 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
        }
        *i = j;
        return Token::Coloured(NUMBER);
    }

    // Decorators / annotations.
    if ch == '@' && chars.get(at + 1).is_some_and(|c| is_word(*c)) {
        let mut j = at + 1;
        while j < chars.len() && is_word(chars[j]) {
            j += 1;
        }
        *i = j;
        return Token::Coloured(DECORATOR);
    }

    // Identifiers, checked against the keyword and builtin sets.
    if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' {
        let mut j = at;
        while j < chars.len() && is_ident(chars[j]) {
            j += 1;
        }
        let word: String = chars[at..j].iter().collect();
        *i = j;
        if KEYWORDS.contains(&word.as_str()) {
            return Token::Coloured(KEYWORD);
        }
        if BUILTINS.contains(&word.as_str()) {
            // Builtins share the number colour — that is what Node emitted.
            return Token::Coloured(NUMBER);
        }
        return Token::Plain;
    }

    Token::None
}

fn is_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn is_ident(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}
