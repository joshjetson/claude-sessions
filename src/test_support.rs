//! Assertions shared by more than one test module. Compiled only under `cfg(test)`.

/// Every `http://` or `https://` reference in a rendered page that would make
/// the browser fetch something.
///
/// Both generated pages — the journal viewer and the pipeline viewer — are
/// opened as `file://` on a laptop that may have no network at all, so a
/// stylesheet, script, font or image pulled from a URL is a bug, not a
/// nicety. Links the reader clicks are fine; resources the page loads are not.
///
/// Deliberately crude: it looks for a URL inside the attributes that cause a
/// load (`src`, `href` on a `<link>`, `url(...)` in CSS) rather than parsing
/// HTML, because a false positive here is cheap and a miss is the whole point.
pub fn external_resource_refs(html: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for (attribute, quoted) in [
        ("src=\"", true),
        ("<link", false),
        ("url(", false),
        ("@import", false),
    ] {
        let mut from = 0;
        while let Some(offset) = html[from..].find(attribute) {
            let start = from + offset;
            from = start + attribute.len();
            let window_end = (start + 400).min(html.len());
            let mut window_end = window_end;
            while !html.is_char_boundary(window_end) {
                window_end -= 1;
            }
            let window = &html[start..window_end];
            let scope = if quoted {
                window.split('"').nth(1).unwrap_or("")
            } else {
                window.split(['>', ';', ')']).next().unwrap_or("")
            };
            if scope.contains("http://") || scope.contains("https://") {
                hits.push(scope.to_string());
            }
        }
    }
    hits
}
