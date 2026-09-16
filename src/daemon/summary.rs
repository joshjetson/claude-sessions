//! An agent's sign-off text as the HTML Odoo's chatter expects.
//!
//! The agent writes a few lines of plain English with light markdown; the
//! chatter renders HTML. The conversion is **escape-first** and that order is
//! the whole point: the summary is text an agent generated from a task it was
//! working, so a stray `<script>` — or a pasted diff full of angle brackets —
//! must land in the comment as characters and never as markup. The tests pin
//! that rather than trusting the reading.

/// `**bold**`, `` `code` ``, blank-line-separated paragraphs with `<br>` inside
/// them, and `- ` bullets as a `<ul>`. Empty in, empty out.
pub fn summary_to_html(markdown: &str) -> String {
    if markdown.trim().is_empty() {
        return String::new();
    }

    let mut html = String::from("<p>📝 <b>Summary</b></p>");
    let mut para: Vec<&str> = Vec::new();
    let mut list: Vec<&str> = Vec::new();

    for raw in markdown.trim().lines() {
        let line = raw.trim_end();
        match bullet_body(line) {
            Some(item) => {
                flush_para(&mut html, &mut para);
                list.push(item);
            }
            None if line.trim().is_empty() => {
                flush_para(&mut html, &mut para);
                flush_list(&mut html, &mut list);
            }
            None => {
                flush_list(&mut html, &mut list);
                para.push(line);
            }
        }
    }
    flush_para(&mut html, &mut para);
    flush_list(&mut html, &mut list);
    html
}

fn flush_para(html: &mut String, para: &mut Vec<&str>) {
    if para.is_empty() {
        return;
    }
    html.push_str("<p>");
    for (index, line) in para.iter().enumerate() {
        if index > 0 {
            html.push_str("<br>");
        }
        html.push_str(&inline(line));
    }
    html.push_str("</p>");
    para.clear();
}

fn flush_list(html: &mut String, list: &mut Vec<&str>) {
    if list.is_empty() {
        return;
    }
    html.push_str("<ul>");
    for item in list.iter() {
        html.push_str("<li>");
        html.push_str(&inline(item));
        html.push_str("</li>");
    }
    html.push_str("</ul>");
    list.clear();
}

/// The text after a `-` or `*` bullet marker, or `None` for an ordinary line.
fn bullet_body(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let body = rest.strip_prefix('-').or_else(|| rest.strip_prefix('*'))?;
    // The marker has to be followed by whitespace, so `-42 degrees` and
    // `*args` stay prose.
    if !body.starts_with(char::is_whitespace) {
        return None;
    }
    Some(body.trim_start())
}

/// Escape, THEN apply the inline markers — so the only tags in the output are
/// the ones this function put there.
fn inline(text: &str) -> String {
    let escaped = escape(text);
    let bolded = wrap_pairs(&escaped, "**", "<b>", "</b>");
    wrap_pairs(&bolded, "`", "<code>", "</code>")
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Replace `<marker>text<marker>` pairs, shortest match first, leaving an
/// unpaired marker as the literal characters the agent typed.
fn wrap_pairs(text: &str, marker: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        // At least one character between the markers, as the non-greedy `.+?`
        // in the original required.
        let end = after
            .char_indices()
            .nth(1)
            .and_then(|(offset, _)| after[offset..].find(marker).map(|at| offset + at));
        match end {
            Some(end) => {
                out.push_str(&rest[..start]);
                out.push_str(open);
                out.push_str(&after[..end]);
                out.push_str(close);
                rest = &after[end + marker.len()..];
            }
            None => {
                out.push_str(&rest[..start + marker.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}
