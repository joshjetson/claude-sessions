//! The journal viewer's page: one self-contained HTML file, no network.
//!
//! Ported from the Node app's `src/journal-html.js`, which was 523 lines
//! because the stylesheet and the client script were string constants inside
//! it. Here they are [`include_str!`]'d from `journal_html/style.css` and
//! `journal_html/app.js` — the same bytes the Node version emitted, so the
//! rendered page behaves identically — and this file is only the shell and the
//! data embedding.
//!
//! Self-contained is a requirement, not a nicety: the page is opened with
//! `file://`, often on a laptop with no VPN, and it has to work. Nothing here
//! emits an `http://` or `https://` resource reference; the data is serialised
//! into a `<script type="application/json">` block and the client reads it from
//! there.

use crate::journal::JournalData;

/// The stylesheet, with its `prefers-color-scheme` dark/light pair.
const CSS: &str = include_str!("journal_html/style.css");
/// The client: tabs, search, filters, master-detail, the mini markdown
/// renderer, and the copy button for each task's `claude --resume` command.
const JS: &str = include_str!("journal_html/app.js");

/// Render the whole page.
pub fn render_html(data: &JournalData) -> String {
    // A literal `</script>` inside entry text would close the data block early;
    // escaping every `<` is the cheap way to make that impossible.
    let json = serde_json::to_string(data)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c");
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Problem Reasoning Journal</title>
<style>{CSS}</style>
</head>
<body>
<header>
  <div class="brand">
    <h1>Problem Reasoning Journal</h1>
    <div class="gen" id="gen"></div>
  </div>
  <nav class="tabs">
    <button class="tab active" data-tab="journal">Journal</button>
    <button class="tab" data-tab="rules">Rules</button>
    <button class="tab" data-tab="tasks">Task archive</button>
    <button class="tab" data-tab="repos">Repos</button>
  </nav>
</header>

<div class="controls">
  <input type="search" id="q" placeholder="Search titles, bodies, rules, task ids…  ( / )" autocomplete="off">
  <select id="repo"></select>
  <select id="range">
    <option value="">All dates</option>
    <option value="7">Last 7 days</option>
    <option value="30">Last 30 days</option>
    <option value="90">Last 90 days</option>
  </select>
  <label class="chk"><input type="checkbox" id="onlyLinked"> linked to an archive</label>
  <label class="chk"><input type="checkbox" id="onlyRev"> revisions only</label>
  <span class="count" id="count"></span>
</div>

<main>
  <section id="list" class="pane list"></section>
  <section id="detail" class="pane detail"><div class="empty">Select an entry.</div></section>
</main>

<script id="data" type="application/json">{json}</script>
<script>{JS}</script>
</body>
</html>"#
    )
}

/// A one-line summary of what was collected, for the CLI.
pub fn summary_line(data: &JournalData) -> String {
    format!(
        "{} entries · {} rules · {} archived tasks",
        data.entries.len(),
        data.rules.len(),
        data.tasks.len()
    )
}

#[cfg(test)]
mod tests;
