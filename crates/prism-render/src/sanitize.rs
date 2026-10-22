// SPDX-License-Identifier: AGPL-3.0-only
//! Sanitization of untrusted markup taken from source documents.
//!
//! A review viewer must never execute content from evidence, and must never
//! make network requests on its behalf (remote images are commonly used as
//! read-receipts and would alert the sender or custodian). Sanitization is the
//! first line of defense; the Content Security Policy emitted by the HTML
//! renderer is the second.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

use ammonia::{Builder, UrlRelative};
use base64::{Engine as _, engine::general_purpose};

/// Prefix given to neutralized hyperlinks so the original target stays
/// visible to the reviewer (e.g. on hover) without being navigable.
pub const BLOCKED_LINK_PREFIX: &str = "#prism-blocked-link:";

/// Attributes that make the browser fetch a resource
const RESOURCE_ATTRIBUTES: &[&str] = &["src", "srcset", "poster", "background", "data"];

static HTML_SANITIZER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut builder = Builder::default();
    builder
        .add_tags([
            "style", "font", "center", "big", "small", "strike", "s", "u", "colgroup", "col",
            "caption", "thead", "tbody", "tfoot",
        ])
        .clean_content_tags(HashSet::from(["script", "title", "noscript", "template"]))
        .add_generic_attributes([
            "style",
            "class",
            "align",
            "valign",
            "bgcolor",
            "color",
            "width",
            "height",
            "border",
            "dir",
            "face",
            "size",
            "cellpadding",
            "cellspacing",
            "colspan",
            "rowspan",
            "nowrap",
        ])
        .id_prefix(Some("doc-"))
        // `filter_attribute` restricts data: URLs to raster images
        .add_url_schemes(["data"])
        .url_relative(UrlRelative::PassThrough)
        .attribute_filter(filter_attribute);
    builder
});

/// Sanitize untrusted HTML for display.
///
/// Scripts, event handlers, forms, frames, plugins and `<meta>`/`<base>`
/// elements are removed. Formatting (inline styles, `<style>` blocks, legacy
/// presentational attributes) is preserved for fidelity. Resource URLs other
/// than inline `data:` images are dropped and hyperlinks are neutralized.
#[must_use]
pub fn sanitize_html(html: &str) -> String {
    HTML_SANITIZER.clean(html).to_string()
}

/// Wrap untrusted SVG markup so that it can be displayed but never execute.
///
/// SVG shown through `<img>` is rendered in "secure animated mode": scripts do
/// not run and no external resources are loaded.
#[must_use]
pub fn svg_as_image(svg: &str, alt: &str) -> String {
    let encoded = general_purpose::STANDARD.encode(svg.as_bytes());
    format!(
        r#"<img class="svg-content" alt="{}" src="data:image/svg+xml;base64,{encoded}">"#,
        crate::html::html_escape(alt)
    )
}

fn filter_attribute<'u>(element: &str, attribute: &str, value: &'u str) -> Option<Cow<'u, str>> {
    if RESOURCE_ATTRIBUTES.contains(&attribute) {
        return is_inline_image(value).then_some(Cow::Borrowed(value));
    }
    if attribute == "style" {
        return Some(strip_remote_css_urls(value));
    }
    if element == "a" && attribute == "href" {
        if value.starts_with('#') {
            return Some(Cow::Borrowed(value));
        }
        return Some(Cow::Owned(format!("{BLOCKED_LINK_PREFIX}{value}")));
    }
    Some(Cow::Borrowed(value))
}

/// Replace every CSS `url(...)` that is not an inline image with `none`, so an
/// inline style cannot make the browser issue a request (the CSP would block
/// it, but the attempt is still visible on the network stack).
fn strip_remote_css_urls(css: &str) -> Cow<'_, str> {
    let lower = css.to_ascii_lowercase();
    if !lower.contains("url(") {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len());
    let mut pos = 0;
    while let Some(start) = lower[pos..].find("url(").map(|i| pos + i) {
        let arg_start = start + 4;
        let end = lower[arg_start..]
            .find(')')
            .map_or(css.len(), |i| arg_start + i + 1);
        out.push_str(&css[pos..start]);
        let arg = css[arg_start..end.min(css.len())]
            .trim_end_matches(')')
            .trim()
            .trim_matches(['"', '\'']);
        if is_inline_image(arg) {
            out.push_str(&css[start..end]);
        } else {
            out.push_str("none");
        }
        pos = end;
    }
    out.push_str(&css[pos..]);
    Cow::Owned(out)
}

fn is_inline_image(url: &str) -> bool {
    let url = url.trim_start();
    url.get(..11)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:image/"))
        && !url
            .get(11..18)
            .is_some_and(|sub| sub.eq_ignore_ascii_case("svg+xml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_scripts_and_handlers() {
        let out = sanitize_html(
            r#"<p onclick="steal()">Hi</p><script>alert(1)</script><img src=x onerror=alert(1)>"#,
        );
        assert!(!out.contains("script"));
        assert!(!out.contains("onclick"));
        assert!(!out.contains("onerror"));
        assert!(out.contains("Hi"));
    }

    #[test]
    fn blocks_remote_images_keeps_inline_images() {
        let out = sanitize_html(
            r#"<img src="https://tracker.example/pixel.gif" alt="t"><img src="data:image/png;base64,AAAA">"#,
        );
        assert!(!out.contains("tracker.example"));
        assert!(out.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn neutralizes_links() {
        let out = sanitize_html(r#"<a href="https://example.com/x">link</a>"#);
        assert!(out.contains(&format!("{BLOCKED_LINK_PREFIX}https://example.com/x")));
        assert!(!out.contains(r#"href="https://"#));
    }

    #[test]
    fn removes_frames_meta_and_forms() {
        let out = sanitize_html(
            r#"<meta http-equiv="refresh" content="0;url=https://x"><base href="https://x/"><iframe src="https://x"></iframe><form action="https://x"><input name=a></form><object data="x"></object>"#,
        );
        for needle in ["<meta", "<base", "<iframe", "<form", "<input", "<object"] {
            assert!(!out.contains(needle), "{needle} survived: {out}");
        }
    }

    #[test]
    fn keeps_formatting() {
        let out = sanitize_html(
            r#"<style>p { color: red }</style><p style="font-weight:bold" class="x">A</p><font face="Arial" color="red">B</font>"#,
        );
        assert!(out.contains("<style>"));
        assert!(out.contains(r#"style="font-weight:bold""#));
        assert!(out.contains(r#"<font face="Arial" color="red">"#));
    }

    #[test]
    fn strips_remote_urls_from_inline_styles() {
        let out = sanitize_html(
            r#"<div style="color: red; background: URL( 'https://t.example/a.gif' )">x</div><p style="background:url(data:image/png;base64,AAAA)">y</p>"#,
        );
        assert!(!out.contains("t.example"), "{out}");
        assert!(out.contains("color: red; background: none"));
        assert!(out.contains("url(data:image/png;base64,AAAA)"));
    }

    #[test]
    fn svg_is_wrapped_as_image() {
        let out = svg_as_image("<svg><script>alert(1)</script></svg>", "a<b");
        assert!(out.starts_with(r#"<img class="svg-content" alt="a&lt;b""#));
        assert!(!out.contains("<script"));
    }
}
