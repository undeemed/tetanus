//! Turning a page into something a model can read.
//!
//! Upstream converts HTML to markdown with turndown, and several of its cases
//! are about that library's edge behaviour - tables, nesting, a depth
//! preflight that stops a pathological document from costing unbounded work.
//! This does less on purpose: it strips markup and keeps the text, with block
//! elements becoming line breaks. The gap is a `docs/parity.md` row and not a
//! silent difference.
//!
//! The scanner is a single pass with no recursion and no backtracking, so the
//! preflight upstream needs against deep nesting has nothing to guard here:
//! the work is linear in the document's length whatever shape it has. A tag
//! that is never closed consumes the rest of the document rather than
//! reopening the text, which is the safe direction - the alternative is markup
//! reaching the model as if it were content.

/// The elements whose content is not text and must not be read as text.
const DROPPED: [&str; 4] = ["script", "style", "noscript", "template"];

/// Elements that end a line when they open or close.
const BREAKING: [&str; 17] = [
    "p",
    "div",
    "br",
    "li",
    "tr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "section",
    "article",
    "header",
    "footer",
    "blockquote",
    "pre",
];

/// Strip markup, keep text, and turn block boundaries into line breaks.
pub fn html_to_text(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 2);
    let mut at = 0usize;

    while at < bytes.len() {
        match bytes[at] {
            b'<' => {
                // A comment runs to `-->`, and everything in it is not text.
                if html[at..].starts_with("<!--") {
                    at = html[at..]
                        .find("-->")
                        .map_or(bytes.len(), |end| at + end + 3);
                    continue;
                }
                let Some(close) = html[at..].find('>') else {
                    // An unterminated tag: the rest of the document is markup
                    // nobody closed, and reading it as text would be worse.
                    break;
                };
                let tag = &html[at + 1..at + close];
                let name = tag_name(tag);
                if DROPPED.contains(&name.as_str()) && !tag.starts_with('/') {
                    at = skip_element(html, at + close + 1, &name);
                    continue;
                }
                if BREAKING.contains(&name.as_str()) {
                    push_break(&mut out);
                }
                at += close + 1;
            }
            b'&' => {
                let (text, used) = entity(&html[at..]);
                out.push_str(&text);
                at += used;
            }
            _ => {
                let char_len = html[at..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&html[at..at + char_len]);
                at += char_len;
            }
        }
    }

    tidy(&out)
}

/// Where an element's closing tag ends, or the end of the document.
fn skip_element(html: &str, from: usize, name: &str) -> usize {
    let closing = format!("</{name}");
    match html[from..].to_ascii_lowercase().find(&closing) {
        None => html.len(),
        Some(at) => {
            let after = from + at;
            html[after..]
                .find('>')
                .map_or(html.len(), |end| after + end + 1)
        }
    }
}

/// The element a tag names, lowercased, without its attributes.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// One entity, and how many bytes of source it took.
///
/// The five XML entities plus the two that appear in every page. Anything else
/// is left as it was written: a model reads `&hellip;` and knows what it
/// means, which is better than a table nobody maintains.
fn entity(text: &str) -> (String, usize) {
    for (name, replacement) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ] {
        if text.starts_with(name) {
            return (replacement.to_string(), name.len());
        }
    }
    ("&".to_string(), 1)
}

fn push_break(out: &mut String) {
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
}

/// Collapse the whitespace a stripped document is full of, without joining
/// lines that were meant to be apart.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() && lines.last().is_some_and(String::is_empty) {
            continue;
        }
        lines.push(collapsed);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    lines.join("\n")
}
