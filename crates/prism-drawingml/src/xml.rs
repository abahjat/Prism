// SPDX-License-Identifier: AGPL-3.0-only
//! Small `quick-xml` helpers shared by the Office readers.

use quick_xml::events::{BytesText, Event};

/// Turn a `GeneralRef` event into a text event; other events pass through unchanged.
///
/// quick-xml 0.42 reports entity and character references (`&amp;`, `&lt;`, `&#10;`) as
/// separate `GeneralRef` events instead of inside the surrounding text, so a reader that only
/// handles `Event::Text` silently drops them (`AT&amp;T` becomes `ATT`, line breaks written as
/// `&#10;` vanish). The returned text is escaped like any other text event, so callers that
/// unescape text keep working. An unknown entity is kept literally (`&name;`).
#[must_use]
pub fn resolve_general_ref(event: Event<'_>) -> Event<'_> {
    let Event::GeneralRef(ref reference) = event else {
        return event;
    };
    let text = if let Ok(Some(ch)) = reference.resolve_char_ref() {
        ch.to_string()
    } else {
        let name: &str = reference.as_ref();
        quick_xml::escape::resolve_predefined_entity(name)
            .map_or_else(|| format!("&{name};"), str::to_string)
    };
    Event::Text(BytesText::from_escaped(
        quick_xml::escape::escape(text.as_str()).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::Reader;

    fn text_of(xml: &str) -> String {
        let mut reader = Reader::from_str(xml);
        let mut out = String::new();
        loop {
            match reader.read_event().map(resolve_general_ref) {
                Ok(Event::Text(t)) => {
                    out.push_str(&quick_xml::escape::unescape(t.as_ref()).unwrap());
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        out
    }

    #[test]
    fn references_become_text() {
        assert_eq!(
            text_of("<t>AT&amp;T&#10;&lt;x&gt; &#x41;</t>"),
            "AT&T\n<x> A"
        );
        assert_eq!(text_of("<t>a&amp;lt;b</t>"), "a&lt;b");
        assert_eq!(text_of("<t>&custom;</t>"), "&custom;");
    }
}
