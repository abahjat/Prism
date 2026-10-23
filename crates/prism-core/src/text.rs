// SPDX-License-Identifier: AGPL-3.0-only
//! Text utilities shared by parsers and renderers.

/// Elements whose content is never visible text
const HIDDEN_ELEMENTS: &[&str] = &["script", "style", "head", "title", "noscript", "template"];

/// Elements that start a new line of text
const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];

/// Convert HTML (or SVG) markup into plain text for extraction and indexing.
///
/// This is a tolerant tokenizer rather than a full HTML parser: tags are
/// dropped, block-level elements become line breaks, table cells become tabs,
/// the content of non-visible elements (`script`, `style`, `head`, ...) is
/// skipped and character references are decoded.
#[must_use]
pub fn html_to_text(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len() / 2);
    let mut hidden_until: Option<String> = None;
    let mut rest = markup;

    while let Some(lt) = rest.find('<') {
        if hidden_until.is_none() {
            push_text(&mut out, &rest[..lt]);
        }
        rest = &rest[lt..];

        // Comments, CDATA and declarations
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after.find("]]>").unwrap_or(after.len());
            if hidden_until.is_none() {
                push_text(&mut out, &after[..end]);
            }
            rest = after.get(end + 3..).unwrap_or("");
            continue;
        }

        let Some(gt) = rest.find('>') else {
            rest = "";
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];

        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '-')
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }

        if let Some(ref until) = hidden_until {
            if closing && *until == name {
                hidden_until = None;
            }
            continue;
        }

        if !closing && !tag.ends_with('/') && HIDDEN_ELEMENTS.contains(&name.as_str()) {
            hidden_until = Some(name);
            continue;
        }

        let separator = if name == "td" || name == "th" {
            (!closing).then_some('\t')
        } else {
            BLOCK_ELEMENTS.contains(&name.as_str()).then_some('\n')
        };
        if let Some(sep) = separator {
            if out.ends_with(' ') {
                out.pop();
            }
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(sep);
            }
        }
    }
    if hidden_until.is_none() {
        push_text(&mut out, rest);
    }

    normalize_whitespace(&out)
}

/// Append decoded text, treating source whitespace as a single space
fn push_text(out: &mut String, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let decoded = decode_entities(raw);
    let mut last_space = out.ends_with([' ', '\n', '\t']);
    for c in decoded.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
}

/// Trim lines and collapse runs of blank lines
fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;
    for line in text.lines() {
        let line = line.trim_matches(|c: char| c == ' ' || c == '\u{a0}');
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Decode HTML character references
fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let entity = &rest[1..semi];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => entity.strip_prefix('#').and_then(|num| {
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, semi))
        });
        if let Some((c, semi)) = decoded {
            out.push(c);
            rest = &rest[semi + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_hidden_content() {
        let html = "<html><head><title>T</title><style>p{color:red}</style></head>\
                    <body><p>Hello <b>World</b></p><script>alert(1)</script><p>Bye</p></body></html>";
        assert_eq!(html_to_text(html), "Hello World\nBye");
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(
            html_to_text("a &amp; b &lt;c&gt; &#65;&#x42; &bogus"),
            "a & b <c> AB &bogus"
        );
    }

    #[test]
    fn tables_become_tabs() {
        let html = "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>";
        assert_eq!(html_to_text(html), "a\tb\nc");
    }

    #[test]
    fn skips_comments() {
        assert_eq!(html_to_text("x<!-- <p>hidden</p> -->y"), "xy");
    }
}
