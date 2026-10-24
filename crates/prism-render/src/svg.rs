// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic SVG page renderer and HTML viewer wrapper for [`LayoutDocument`].
//!
//! Glyphs are emitted as normalized vector outlines in `<defs>` (`1000.0` units per em)
//! and instantiated via `<use href="#p{page}-f{font}-g{glyph}"/>`. No web fonts or network
//! requests are required. Every element id carries a per-page prefix so pages can be
//! inlined into one HTML document without their glyph, gradient and clip ids colliding. The HTML viewer wrapper overlays an invisible, selectable
//! text layer constructed from [`LayoutPage::char_boxes`].

use base64::{Engine as _, engine::general_purpose};
use prism_core::error::{Error, Result};
use prism_layout::{
    Affine, CharBox, ClipPath, FillPaint, FillRule, FontId, FontStore, GlyphRun, GroupItem,
    ImageItem, ImageStore, Item, LayoutDocument, LayoutOptions, LayoutPage, LineCap,
    LineDecoration, LineJoin, MarkupMode, NORMALIZED_UPEM, PathCommand, PathItem, StrokeStyle,
    normalize_zero,
};
use std::collections::BTreeSet;
use std::fmt::Write as _;

const PRISM_VIEWER_JS: &str = include_str!("../assets/prism-viewer.js");
const DEFAULT_VIEWER_NONCE: &str = "prism-viewer-v1";

/// Configuration options for rendering a [`LayoutDocument`] to SVG / HTML viewer output.
#[derive(Debug, Clone)]
pub struct SvgViewerOptions {
    /// Document title shown in the HTML `<title>` element.
    pub title: Option<String>,
    /// Whether to draw an opaque white page background rectangle behind page items.
    pub white_background: bool,
    /// Resource limits enforced during rendering.
    pub layout_options: LayoutOptions,
}

impl Default for SvgViewerOptions {
    fn default() -> Self {
        Self {
            title: None,
            white_background: true,
            layout_options: LayoutOptions::default(),
        }
    }
}

struct SvgBuildState<'a> {
    fonts: &'a FontStore,
    images: &'a ImageStore,
    options: &'a LayoutOptions,
    used_glyphs: BTreeSet<(FontId, u16)>,
    extra_defs: Vec<String>,
    glyph_count: usize,
    /// Per-page id prefix (`p{index}-`) keeping ids unique in the multi-page viewer.
    id_prefix: String,
}

/// Render a single [`LayoutPage`] as a standalone, deterministic SVG document string.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits (`max_glyphs_per_page` or `max_recursion_depth`)
/// are exceeded.
pub fn render_page_svg(
    page: &LayoutPage,
    fonts: &FontStore,
    images: &ImageStore,
    options: &LayoutOptions,
) -> Result<String> {
    render_page_svg_with_bg(page, fonts, images, options, true)
}

/// Render a single [`LayoutPage`] as a standalone SVG string, optionally omitting the white page sheet rect.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits (`max_glyphs_per_page` or `max_recursion_depth`)
/// are exceeded.
pub fn render_page_svg_with_bg(
    page: &LayoutPage,
    fonts: &FontStore,
    images: &ImageStore,
    options: &LayoutOptions,
    white_background: bool,
) -> Result<String> {
    let mut state = SvgBuildState {
        fonts,
        images,
        options,
        used_glyphs: BTreeSet::new(),
        extra_defs: Vec::new(),
        glyph_count: 0,
        id_prefix: format!("p{}-", page.index),
    };

    let mut body = String::new();
    render_items(&page.items, 0, &mut state, &mut body)?;

    let width = fmt_num(page.size.width.max(1.0));
    let height = fmt_num(page.size.height.max(1.0));

    let mut svg = String::with_capacity(body.len() + 1024);
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" data-page="{}">"#,
        page.index
    );

    if !state.used_glyphs.is_empty() || !state.extra_defs.is_empty() {
        svg.push_str("<defs>\n");
        for (font_id, glyph_id) in &state.used_glyphs {
            if let Some(commands) = fonts.glyph_outline(*font_id, *glyph_id)
                && !commands.is_empty()
            {
                let d = format_path_commands(&commands);
                let _ = writeln!(
                    svg,
                    r#"<path id="{}f{}-g{glyph_id}" d="{d}"/>"#,
                    state.id_prefix, font_id.0
                );
            }
        }
        for def in &state.extra_defs {
            svg.push_str(def);
            svg.push('\n');
        }
        svg.push_str("</defs>\n");
    }

    if white_background {
        let _ = writeln!(
            svg,
            r##"<rect class="prism-page-bg" x="0" y="0" width="{width}" height="{height}" fill="#FFFFFF"/>"##
        );
    }

    svg.push_str(&body);
    svg.push_str("</svg>");
    Ok(svg)
}

/// Render a single [`LayoutPage`] as an HTML `<section class="prism-page">` containing both
/// the SVG vector layer and the invisible, selectable HTML text overlay.
///
/// # Errors
///
/// Returns an [`Error`] if resource limits are exceeded.
pub fn render_page_viewer_fragment(
    page: &LayoutPage,
    fonts: &FontStore,
    images: &ImageStore,
    options: &LayoutOptions,
) -> Result<String> {
    let svg = render_page_svg(page, fonts, images, options)?;
    let char_boxes = page.char_boxes();
    let text_layer = render_selectable_text_layer(&char_boxes);
    let width = fmt_num(page.size.width.max(1.0));
    let height = fmt_num(page.size.height.max(1.0));

    let mut out = String::with_capacity(svg.len() + text_layer.len() + 256);
    let page_num = page.index + 1;
    let _ = writeln!(
        out,
        r#"<section class="prism-page" data-page="{}" data-width-pt="{width}" data-height-pt="{height}" role="region" aria-label="Page {page_num}" style="width:{width}pt;height:{height}pt;">"#,
        page.index
    );
    out.push_str(&svg);
    out.push('\n');
    out.push_str(&text_layer);
    out.push_str("\n</section>");
    Ok(out)
}

/// Render a complete [`LayoutDocument`] as a self-contained HTML5 viewer document with
/// per-page `<svg>` + selectable text layers, interactive toolbar (navigation, zoom,
/// rotation, thumbnails, search), and a strict Content Security Policy.
///
/// # Errors
///
/// Returns an [`Error`] if `doc.pages.len() > options.layout_options.max_pages` or if any
/// page exceeds per-page resource limits.
#[allow(clippy::too_many_lines)]
pub fn render_document_viewer_html(
    doc: &LayoutDocument,
    options: &SvgViewerOptions,
) -> Result<String> {
    if doc.pages.len() > options.layout_options.max_pages {
        return Err(Error::InvalidInput(format!(
            "Document page count ({}) exceeds maximum allowed pages ({})",
            doc.pages.len(),
            options.layout_options.max_pages
        )));
    }

    let title = xml_escape(options.title.as_deref().unwrap_or("Prism Document"));
    let nonce = DEFAULT_VIEWER_NONCE;
    let csp = format!(
        "default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'nonce-{nonce}'; form-action 'none'; base-uri 'none'; frame-src 'none'; object-src 'none'"
    );
    let page_count = doc.pages.len();
    let markup_options = markup_mode_options(options.layout_options.markup_mode);

    let mut html = String::new();
    let _ = write!(
        html,
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="{csp}">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title>
<style>
* {{ box-sizing: border-box; }}
body {{
  margin: 0;
  padding: 0;
  background: #e5e7eb;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  color: #1f2937;
}}
.prism-app-shell {{
  display: flex;
  flex-direction: column;
  height: 100vh;
  width: 100vw;
  overflow: hidden;
}}
.prism-toolbar {{
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 6px 12px;
  background: #1e293b;
  color: #f8fafc;
  border-bottom: 1px solid #334155;
  z-index: 20;
}}
.prism-toolbar-group {{
  display: flex;
  align-items: center;
  gap: 6px;
}}
.prism-toolbar button,
.prism-toolbar select,
.prism-toolbar input[type="number"],
.prism-toolbar input[type="search"] {{
  background: #334155;
  color: #f8fafc;
  border: 1px solid #475569;
  border-radius: 4px;
  padding: 4px 8px;
  font-size: 13px;
}}
.prism-toolbar button:hover {{
  background: #475569;
  cursor: pointer;
}}
.prism-toolbar label {{
  font-size: 12px;
  display: inline-flex;
  align-items: center;
  gap: 3px;
}}
.prism-body {{
  display: flex;
  flex: 1;
  min-height: 0;
  overflow: hidden;
}}
.prism-thumbs-sidebar {{
  width: 160px;
  background: #f1f5f9;
  border-right: 1px solid #cbd5e1;
  overflow-y: auto;
  padding: 12px 8px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}}
.prism-thumbs-sidebar[hidden] {{
  display: none;
}}
.prism-thumb-item {{
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 6px;
  background: transparent;
  border: 2px solid transparent;
  border-radius: 6px;
  cursor: pointer;
}}
.prism-thumb-item.prism-thumb-active {{
  border-color: #2563eb;
  background: #dbeafe;
}}
.prism-thumb-box {{
  display: block;
  width: 96px;
  background: #ffffff;
  border: 1px solid #94a3b8;
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.12);
}}
.prism-thumb-label {{
  font-size: 12px;
  color: #334155;
}}
.prism-viewport {{
  flex: 1;
  overflow: auto;
  padding: 16px;
}}
.prism-viewer {{
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 16px;
}}
.prism-page {{
  position: relative;
  background: #ffffff;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.15);
  overflow: hidden;
  flex-shrink: 0;
}}
.prism-page > svg {{
  display: block;
  width: 100%;
  height: 100%;
}}
.prism-text-layer {{
  position: absolute;
  inset: 0;
  pointer-events: auto;
  user-select: text;
  -webkit-user-select: text;
}}
.prism-text-span {{
  position: absolute;
  color: transparent;
  white-space: pre;
  line-height: 1;
  transform-origin: 0 0;
}}
.prism-text-span::selection {{
  background: rgba(0, 102, 204, 0.28);
  color: transparent;
}}
.prism-search-layer {{
  position: absolute;
  inset: 0;
  pointer-events: none;
}}
.prism-search-hit {{
  position: absolute;
  background: rgba(250, 204, 21, 0.45);
  outline: 1px solid rgba(202, 138, 4, 0.8);
}}
.prism-search-hit-active {{
  background: rgba(249, 115, 22, 0.55);
  outline: 2px solid #ea580c;
}}
.prism-formula-bar {{
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 12px;
  background: #f8fafc;
  border-bottom: 1px solid #cbd5e1;
  font-size: 12px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}}
.prism-formula-bar[hidden],
.prism-sheet-tabs[hidden] {{
  display: none;
}}
.prism-cell-ref {{
  min-width: 64px;
  font-weight: 600;
  padding: 2px 6px;
  background: #e2e8f0;
  border-radius: 3px;
  text-align: center;
}}
.prism-formula-expr {{
  flex: 1;
  padding: 2px 6px;
  background: #ffffff;
  border: 1px solid #cbd5e1;
  border-radius: 3px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}}
.prism-cell-meta {{
  color: #475569;
  font-size: 11px;
}}
.prism-sheet-tabs {{
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px 12px;
  background: #e2e8f0;
  border-top: 1px solid #cbd5e1;
  overflow-x: auto;
}}
.prism-sheet-tab {{
  padding: 4px 10px;
  border: 1px solid #cbd5e1;
  border-bottom: none;
  border-radius: 4px 4px 0 0;
  background: #f1f5f9;
  color: #334155;
  font-size: 12px;
  cursor: pointer;
}}
.prism-sheet-tab.prism-sheet-tab-active {{
  background: #ffffff;
  color: #0f172a;
  font-weight: 600;
}}
.prism-sheet-badge {{
  margin-left: 4px;
  padding: 1px 4px;
  border-radius: 3px;
  font-size: 10px;
  background: #fef3c7;
  color: #92400e;
}}
.prism-review-panel {{
  width: 280px;
  background: #f8fafc;
  border-left: 1px solid #cbd5e1;
  overflow-y: auto;
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  font-size: 12px;
  color: #1e293b;
}}
.prism-review-panel[hidden] {{
  display: none;
}}
.prism-review-group {{
  background: #ffffff;
  border: 1px solid #e2e8f0;
  border-radius: 6px;
  padding: 8px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}}
.prism-review-group-header {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-weight: 600;
  color: #0f172a;
}}
.prism-finding-item {{
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 6px;
  border: 1px solid #cbd5e1;
  border-radius: 4px;
  background: #f8fafc;
  cursor: pointer;
  text-align: left;
  width: 100%;
}}
.prism-finding-item:hover {{
  background: #eff6ff;
  border-color: #60a5fa;
}}
.prism-finding-severity {{
  display: inline-block;
  padding: 1px 5px;
  border-radius: 3px;
  font-size: 10px;
  font-weight: 600;
  text-transform: uppercase;
}}
.prism-severity-high {{
  background: #fee2e2;
  color: #991b1b;
}}
.prism-severity-notable {{
  background: #fef3c7;
  color: #92400e;
}}
.prism-severity-info {{
  background: #e0f2fe;
  color: #075985;
}}
.prism-finding-layer,
.prism-annotation-layer {{
  position: absolute;
  inset: 0;
  pointer-events: none;
}}
.prism-finding-highlight {{
  position: absolute;
  background: rgba(239, 68, 68, 0.28);
  outline: 2px dashed #dc2626;
}}
.prism-annotation-box {{
  position: absolute;
  box-sizing: border-box;
}}
.prism-redaction-box {{
  position: absolute;
  background: #000000;
  color: #ffffff;
  font-size: 9px;
  font-weight: 700;
  display: flex;
  align-items: center;
  justify-content: center;
  overflow: hidden;
  box-sizing: border-box;
}}
.prism-redaction-proposed {{
  background: rgba(0, 0, 0, 0.62);
  outline: 2px dashed #dc2626;
}}
</style>
</head>
<body>
<div class="prism-app-shell">
<header class="prism-toolbar" role="toolbar" aria-label="Document viewer controls">
  <div class="prism-toolbar-group">
    <button type="button" id="prism-btn-thumbs" aria-label="Toggle page thumbnails" title="Thumbnails">Thumbnails</button>
    <button type="button" id="prism-btn-prev" aria-label="Previous page" title="Previous page">&larr;</button>
    <input type="number" id="prism-page-input" aria-label="Current page" value="1" min="1" max="{page_count}" style="width:56px">
    <span>/ <span id="prism-page-total">{page_count}</span></span>
    <button type="button" id="prism-btn-next" aria-label="Next page" title="Next page">&rarr;</button>
  </div>
  <div class="prism-toolbar-group">
    <button type="button" id="prism-btn-zoom-out" aria-label="Zoom out" title="Zoom out">&minus;</button>
    <select id="prism-zoom-select" aria-label="Zoom level">
      <option value="fit-width" selected>Fit Width</option>
      <option value="fit-page">Fit Page</option>
      <option value="0.5">50%</option>
      <option value="0.75">75%</option>
      <option value="1">100%</option>
      <option value="1.25">125%</option>
      <option value="1.5">150%</option>
      <option value="2">200%</option>
      <option value="custom" hidden>Custom</option>
    </select>
    <button type="button" id="prism-btn-zoom-in" aria-label="Zoom in" title="Zoom in">+</button>
    <button type="button" id="prism-btn-rotate-ccw" aria-label="Rotate counter-clockwise" title="Rotate counter-clockwise">&#8634;</button>
    <button type="button" id="prism-btn-rotate-cw" aria-label="Rotate clockwise" title="Rotate clockwise">&#8635;</button>
    <select id="prism-markup-select" aria-label="Tracked changes markup mode" title="Tracked changes markup mode" hidden>
{markup_options}    </select>
    <label id="prism-forensic-hidden-wrap" hidden><input type="checkbox" id="prism-toggle-hidden-sheets">Hidden Sheets/Rows</label>
    <label id="prism-forensic-concealed-wrap" hidden><input type="checkbox" id="prism-toggle-concealed">Reveal ;;;</label>
  </div>
  <div class="prism-toolbar-group" role="search" aria-label="Find in document">
    <input type="search" id="prism-search-input" aria-label="Search document" placeholder="Find in document...">
    <label><input type="checkbox" id="prism-search-case" aria-label="Match case">Aa</label>
    <label><input type="checkbox" id="prism-search-word" aria-label="Whole word">Word</label>
    <button type="button" id="prism-search-prev" aria-label="Previous match" title="Previous match">&uarr;</button>
    <button type="button" id="prism-search-next" aria-label="Next match" title="Next match">&darr;</button>
    <span id="prism-search-status" role="status" aria-live="polite"></span>
    <button type="button" id="prism-btn-propose-redact" aria-label="Propose redactions from search hits" title="Propose redactions from search hits" hidden>Redact Hits</button>
    <label id="prism-preview-redact-wrap" hidden><input type="checkbox" id="prism-toggle-preview-redact" checked>Preview Redactions</label>
    <button type="button" id="prism-btn-review" aria-label="Toggle review and findings panel" title="Review &amp; Findings">Review</button>
  </div>
</header>
<div class="prism-formula-bar" id="prism-formula-bar" role="region" aria-label="Formula and cell inspector" hidden>
  <span class="prism-cell-ref" id="prism-cell-ref">A1</span>
  <span class="prism-formula-expr" id="prism-formula-expr"></span>
  <span class="prism-cell-meta" id="prism-cell-meta"></span>
</div>
<div class="prism-body">
<aside class="prism-thumbs-sidebar" role="navigation" aria-label="Page thumbnails" hidden></aside>
<div class="prism-viewport">
<main class="prism-viewer">
"#
    );

    for page in &doc.pages {
        let fragment =
            render_page_viewer_fragment(page, &doc.fonts, &doc.images, &options.layout_options)?;
        html.push_str(&fragment);
        html.push('\n');
    }

    let _ = write!(
        html,
        "</main>\n</div>\n<aside class=\"prism-review-panel\" id=\"prism-review-panel\" role=\"complementary\" aria-label=\"Hidden content findings and annotations\" hidden></aside>\n</div>\n<footer class=\"prism-sheet-tabs\" id=\"prism-sheet-tabs\" role=\"tablist\" aria-label=\"Workbook sheets\" hidden></footer>\n</div>\n<script nonce=\"{nonce}\">\n{PRISM_VIEWER_JS}\n</script>\n</body>\n</html>\n"
    );
    Ok(html)
}

fn render_items(
    items: &[Item],
    depth: u32,
    state: &mut SvgBuildState<'_>,
    out: &mut String,
) -> Result<()> {
    if depth > state.options.max_recursion_depth {
        return Err(Error::InvalidInput(format!(
            "Group nesting depth ({depth}) exceeds max_recursion_depth ({})",
            state.options.max_recursion_depth
        )));
    }

    for item in items {
        match item {
            Item::Glyphs(run) => render_glyph_run(run, state, out)?,
            Item::Path(path) => render_path_item(path, state, out),
            Item::Image(img) => render_image_item(img, state, out),
            Item::Group(group) => render_group_item(group, depth, state, out)?,
            Item::Link(link) => {
                let target = xml_escape(&link.target);
                let _ = writeln!(
                    out,
                    r#"<rect class="prism-link" x="{}" y="{}" width="{}" height="{}" fill="transparent" data-link-target="{target}"><title>{target}</title></rect>"#,
                    fmt_num(link.rect.x),
                    fmt_num(link.rect.y),
                    fmt_num(link.rect.width),
                    fmt_num(link.rect.height)
                );
            }
            Item::Anchor(anchor) => {
                let name = xml_escape(&anchor.name);
                let _ = writeln!(
                    out,
                    r#"<rect class="prism-anchor" x="{}" y="{}" width="{}" height="{}" fill="transparent" data-anchor="{name}"/>"#,
                    fmt_num(anchor.rect.x),
                    fmt_num(anchor.rect.y),
                    fmt_num(anchor.rect.width),
                    fmt_num(anchor.rect.height)
                );
            }
        }
    }
    Ok(())
}

fn render_glyph_run(run: &GlyphRun, state: &mut SvgBuildState<'_>, out: &mut String) -> Result<()> {
    state.glyph_count = state.glyph_count.saturating_add(run.glyphs.len());
    if state.glyph_count > state.options.max_glyphs_per_page {
        return Err(Error::InvalidInput(format!(
            "Page glyph count ({}) exceeds max_glyphs_per_page ({})",
            state.glyph_count, state.options.max_glyphs_per_page
        )));
    }

    if run.glyphs.is_empty()
        && run.decorations.highlight.is_none()
        && run.decorations.underline.is_none()
        && run.decorations.strikethrough.is_none()
    {
        return Ok(());
    }

    let run_advance = run
        .glyphs
        .iter()
        .fold(0.0_f32, |acc, g| acc.max(g.x + g.advance))
        .max(
            run.clusters
                .iter()
                .fold(0.0_f32, |acc, c| acc.max(c.x + c.advance)),
        );

    let transform_attr = format_transform_attr(run.transform);
    let fill_hex = run.color.to_hex_rgb();
    let opacity_attr = if run.color.a < 255 {
        format!(r#" fill-opacity="{}""#, fmt_num(run.color.opacity()))
    } else {
        String::new()
    };

    let _ = writeln!(
        out,
        r#"<g{transform_attr} fill="{fill_hex}"{opacity_attr}>"#
    );

    if let Some(hl) = run.decorations.highlight
        && run_advance > 0.0
    {
        let hl_hex = hl.to_hex_rgb();
        let hl_op = if hl.a < 255 {
            format!(r#" fill-opacity="{}""#, fmt_num(hl.opacity()))
        } else {
            String::new()
        };
        let ascent = run.size * 0.8;
        let _ = writeln!(
            out,
            r#"<rect x="0" y="{}" width="{}" height="{}" fill="{hl_hex}"{hl_op}/>"#,
            fmt_num(-ascent),
            fmt_num(run_advance),
            fmt_num(run.size)
        );
    }

    let scale = run.size.max(0.1) / NORMALIZED_UPEM;
    for glyph in &run.glyphs {
        if state
            .fonts
            .glyph_outline(run.font, glyph.glyph_id)
            .is_some_and(|cmds| !cmds.is_empty())
        {
            state.used_glyphs.insert((run.font, glyph.glyph_id));
            let glyph_transform = Affine::translate(glyph.x, glyph.y).then(Affine::scale(scale));
            let gt_attr = format_transform_attr(glyph_transform);
            let _ = writeln!(
                out,
                r##"<use href="#{}f{}-g{}"{gt_attr}/>"##,
                state.id_prefix, run.font.0, glyph.glyph_id
            );
        }
    }

    if let Some(ul) = run.decorations.underline
        && run_advance > 0.0
    {
        render_line_decoration(ul, run_advance, out);
    }
    if let Some(st) = run.decorations.strikethrough
        && run_advance > 0.0
    {
        render_line_decoration(st, run_advance, out);
    }

    out.push_str("</g>\n");
    Ok(())
}

fn render_line_decoration(dec: LineDecoration, width: f32, out: &mut String) {
    let hex = dec.color.to_hex_rgb();
    let op = if dec.color.a < 255 {
        format!(r#" fill-opacity="{}""#, fmt_num(dec.color.opacity()))
    } else {
        String::new()
    };
    let y = dec.offset - dec.thickness * 0.5;
    let _ = writeln!(
        out,
        r#"<rect x="0" y="{}" width="{}" height="{}" fill="{hex}"{op}/>"#,
        fmt_num(y),
        fmt_num(width),
        fmt_num(dec.thickness.max(0.25))
    );
}

fn render_path_item(path: &PathItem, state: &mut SvgBuildState<'_>, out: &mut String) {
    if path.commands.is_empty() {
        return;
    }
    let d = format_path_commands(&path.commands);
    let transform_attr = format_transform_attr(path.transform);

    let mut attrs = String::new();
    match &path.fill {
        None => attrs.push_str(r#" fill="none""#),
        Some(FillPaint::Solid(color)) => {
            let _ = write!(attrs, r#" fill="{}""#, color.to_hex_rgb());
            if color.a < 255 {
                let _ = write!(attrs, r#" fill-opacity="{}""#, fmt_num(color.opacity()));
            }
        }
        Some(FillPaint::LinearGradient(grad)) => {
            let grad_id = format!("{}grad-{}", state.id_prefix, state.extra_defs.len());
            let mut def = format!(
                r#"<linearGradient id="{grad_id}" gradientUnits="userSpaceOnUse" x1="{}" y1="{}" x2="{}" y2="{}">"#,
                fmt_num(grad.start.x),
                fmt_num(grad.start.y),
                fmt_num(grad.end.x),
                fmt_num(grad.end.y)
            );
            for stop in &grad.stops {
                let _ = write!(
                    def,
                    r#"<stop offset="{}" stop-color="{}" stop-opacity="{}"/>"#,
                    fmt_num(stop.offset.clamp(0.0, 1.0)),
                    stop.color.to_hex_rgb(),
                    fmt_num(stop.color.opacity())
                );
            }
            def.push_str("</linearGradient>");
            state.extra_defs.push(def);
            let _ = write!(attrs, r#" fill="url(#{grad_id})""#);
        }
        Some(FillPaint::RadialGradient(grad)) => {
            let grad_id = format!("{}grad-{}", state.id_prefix, state.extra_defs.len());
            let mut def = format!(
                r#"<radialGradient id="{grad_id}" gradientUnits="userSpaceOnUse" cx="{}" cy="{}" r="{}">"#,
                fmt_num(grad.center.x),
                fmt_num(grad.center.y),
                fmt_num(grad.radius.max(0.0))
            );
            for stop in &grad.stops {
                let _ = write!(
                    def,
                    r#"<stop offset="{}" stop-color="{}" stop-opacity="{}"/>"#,
                    fmt_num(stop.offset.clamp(0.0, 1.0)),
                    stop.color.to_hex_rgb(),
                    fmt_num(stop.color.opacity())
                );
            }
            def.push_str("</radialGradient>");
            state.extra_defs.push(def);
            let _ = write!(attrs, r#" fill="url(#{grad_id})""#);
        }
    }

    if path.fill.is_some() && path.fill_rule == FillRule::EvenOdd {
        attrs.push_str(r#" fill-rule="evenodd""#);
    }

    if let Some(stroke) = &path.stroke {
        append_stroke_attrs(stroke, &mut attrs);
    }

    let _ = writeln!(out, r#"<path d="{d}"{transform_attr}{attrs}/>"#);
}

fn append_stroke_attrs(stroke: &StrokeStyle, attrs: &mut String) {
    let _ = write!(
        attrs,
        r#" stroke="{}" stroke-width="{}""#,
        stroke.color.to_hex_rgb(),
        fmt_num(stroke.width.max(0.0))
    );
    if stroke.color.a < 255 {
        let _ = write!(
            attrs,
            r#" stroke-opacity="{}""#,
            fmt_num(stroke.color.opacity())
        );
    }
    match stroke.line_cap {
        LineCap::Butt => {}
        LineCap::Round => attrs.push_str(r#" stroke-linecap="round""#),
        LineCap::Square => attrs.push_str(r#" stroke-linecap="square""#),
    }
    match stroke.line_join {
        LineJoin::Miter => {}
        LineJoin::Round => attrs.push_str(r#" stroke-linejoin="round""#),
        LineJoin::Bevel => attrs.push_str(r#" stroke-linejoin="bevel""#),
    }
    if !stroke.dash_array.is_empty() {
        let dashes: Vec<String> = stroke.dash_array.iter().map(|&v| fmt_num(v)).collect();
        let _ = write!(attrs, r#" stroke-dasharray="{}""#, dashes.join(" "));
        if stroke.dash_offset.abs() > 1e-4 {
            let _ = write!(
                attrs,
                r#" stroke-dashoffset="{}""#,
                fmt_num(stroke.dash_offset)
            );
        }
    }
}

fn render_image_item(img: &ImageItem, state: &SvgBuildState<'_>, out: &mut String) {
    let Some(entry) = state.images.get(img.image) else {
        return;
    };
    let encoded = general_purpose::STANDARD.encode(&entry.data);
    let transform_attr = format_transform_attr(img.transform);
    let opacity_attr = if img.opacity < 1.0 - 1e-4 {
        format!(r#" opacity="{}""#, fmt_num(img.opacity.clamp(0.0, 1.0)))
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        r#"<image x="{}" y="{}" width="{}" height="{}" preserveAspectRatio="none" href="data:{};base64,{encoded}"{transform_attr}{opacity_attr}/>"#,
        fmt_num(img.rect.x),
        fmt_num(img.rect.y),
        fmt_num(img.rect.width),
        fmt_num(img.rect.height),
        xml_escape(&entry.mime_type)
    );
}

fn render_group_item(
    group: &GroupItem,
    depth: u32,
    state: &mut SvgBuildState<'_>,
    out: &mut String,
) -> Result<()> {
    if group.children.is_empty() {
        return Ok(());
    }
    let transform_attr = format_transform_attr(group.transform);
    let opacity_attr = if group.opacity > 0.0 && group.opacity < 1.0 - 1e-4 {
        format!(r#" opacity="{}""#, fmt_num(group.opacity.clamp(0.0, 1.0)))
    } else {
        String::new()
    };
    let clip_attr = if let Some(ClipPath {
        commands,
        fill_rule,
    }) = &group.clip
        && !commands.is_empty()
    {
        let clip_id = format!("{}clip-{}", state.id_prefix, state.extra_defs.len());
        let d = format_path_commands(commands);
        let rule = if *fill_rule == FillRule::EvenOdd {
            r#" clip-rule="evenodd""#
        } else {
            ""
        };
        state.extra_defs.push(format!(
            r#"<clipPath id="{clip_id}"><path d="{d}"{rule}/></clipPath>"#
        ));
        format!(r#" clip-path="url(#{clip_id})""#)
    } else {
        String::new()
    };

    let _ = writeln!(out, "<g{transform_attr}{clip_attr}{opacity_attr}>");
    render_items(&group.children, depth + 1, state, out)?;
    out.push_str("</g>\n");
    Ok(())
}

fn render_selectable_text_layer(char_boxes: &[CharBox]) -> String {
    let mut out = String::from(r#"<div class="prism-text-layer">"#);
    if char_boxes.is_empty() {
        out.push_str("</div>");
        return out;
    }

    let mut span_text = String::new();
    let mut span_rect = char_boxes[0].rect;
    let mut span_font_size = char_boxes[0].font_size;

    for cb in char_boxes {
        if cb.ch == '\n' {
            flush_text_span(&mut out, &mut span_text, span_rect, span_font_size);
            continue;
        }
        if span_text.is_empty() {
            span_rect = cb.rect;
            span_font_size = cb.font_size;
            span_text.push(cb.ch);
        } else {
            let dy = (cb.rect.y - span_rect.y).abs();
            let threshold = span_font_size.max(cb.font_size) * 0.45;
            if dy <= threshold {
                span_rect = span_rect.union(cb.rect);
                span_text.push(cb.ch);
            } else {
                flush_text_span(&mut out, &mut span_text, span_rect, span_font_size);
                span_rect = cb.rect;
                span_font_size = cb.font_size;
                span_text.push(cb.ch);
            }
        }
    }
    flush_text_span(&mut out, &mut span_text, span_rect, span_font_size);
    out.push_str("</div>");
    out
}

fn flush_text_span(out: &mut String, text: &mut String, rect: prism_layout::Rect, font_size: f32) {
    if text.is_empty() {
        return;
    }
    let escaped = xml_escape(text);
    let _ = write!(
        out,
        r#"<span class="prism-text-span" style="left:{}pt;top:{}pt;width:{}pt;height:{}pt;font-size:{}pt;">{escaped}</span>"#,
        fmt_num(rect.x),
        fmt_num(rect.y),
        fmt_num(rect.width),
        fmt_num(rect.height),
        fmt_num(font_size)
    );
    text.clear();
}

/// Format a slice of [`PathCommand`]s into a deterministic SVG path `d` attribute string.
#[must_use]
pub fn format_path_commands(commands: &[PathCommand]) -> String {
    let mut out = String::with_capacity(commands.len() * 16);
    for (idx, cmd) in commands.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        match *cmd {
            PathCommand::MoveTo(pt) => {
                let _ = write!(out, "M {} {}", fmt_num(pt.x), fmt_num(pt.y));
            }
            PathCommand::LineTo(pt) => {
                let _ = write!(out, "L {} {}", fmt_num(pt.x), fmt_num(pt.y));
            }
            PathCommand::QuadTo { ctrl, to } => {
                let _ = write!(
                    out,
                    "Q {} {} {} {}",
                    fmt_num(ctrl.x),
                    fmt_num(ctrl.y),
                    fmt_num(to.x),
                    fmt_num(to.y)
                );
            }
            PathCommand::CurveTo { ctrl1, ctrl2, to } => {
                let _ = write!(
                    out,
                    "C {} {} {} {} {} {}",
                    fmt_num(ctrl1.x),
                    fmt_num(ctrl1.y),
                    fmt_num(ctrl2.x),
                    fmt_num(ctrl2.y),
                    fmt_num(to.x),
                    fmt_num(to.y)
                );
            }
            PathCommand::Close => out.push('Z'),
        }
    }
    out
}

fn format_transform_attr(transform: Affine) -> String {
    if transform.is_identity() {
        return String::new();
    }
    let [a, b, c, d, tx, ty] = transform.coeffs;
    if (a - 1.0).abs() <= 1e-6 && b.abs() <= 1e-6 && c.abs() <= 1e-6 && (d - 1.0).abs() <= 1e-6 {
        return format!(r#" transform="translate({} {})""#, fmt_num(tx), fmt_num(ty));
    }
    format!(
        r#" transform="matrix({} {} {} {} {} {})""#,
        fmt_num(a),
        fmt_num(b),
        fmt_num(c),
        fmt_num(d),
        fmt_num(tx),
        fmt_num(ty)
    )
}

fn fmt_num(val: f32) -> String {
    let n = normalize_zero(val);
    let formatted = format!("{n:.3}");
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `<option>` list for the viewer's markup selector, with the mode the pages were laid out in
/// selected. The selector stays hidden unless a native host can re-lay out the document.
fn markup_mode_options(current: MarkupMode) -> String {
    let modes = [
        (MarkupMode::AllMarkup, "All Markup"),
        (MarkupMode::SimpleMarkup, "Simple Markup"),
        (MarkupMode::NoMarkup, "No Markup"),
        (MarkupMode::Original, "Original"),
    ];
    let mut out = String::new();
    for (value, (mode, label)) in modes.into_iter().enumerate() {
        let selected = if mode == current { " selected" } else { "" };
        let _ = writeln!(
            out,
            r#"      <option value="{value}"{selected}>{label}</option>"#
        );
    }
    out
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_layout::{
        FontSystem, LineSpacing, ParagraphSpec, Point, Rgba, Size, SpanSpec, TextAlign,
        TextLayoutEngine,
    };

    #[test]
    fn test_svg_page_and_viewer_determinism() {
        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut engine = TextLayoutEngine::new();

        let spec = ParagraphSpec {
            spans: vec![SpanSpec {
                text: "Deterministic SVG Output".to_string(),
                family: "Calibri".to_string(),
                size: 14.0,
                bold: true,
                color: Rgba::rgb(0x10, 0x20, 0x30),
                underline: true,
                ..SpanSpec::default()
            }],
            align: TextAlign::Left,
            line_spacing: LineSpacing::Multiple(1.2),
            max_width: Some(400.0),
            first_line_indent: 0.0,
            origin: Point::new(50.0, 60.0),
            page_index: 0,
            block_index: 0,
        };

        let shaped = engine.layout_paragraph(&spec, &mut font_sys);
        let mut page = LayoutPage::new(0, Size::LETTER);
        for run in shaped.runs {
            page.items.push(Item::Glyphs(run));
        }

        let doc = LayoutDocument {
            pages: vec![page],
            fonts: font_sys.store,
            images: ImageStore::new(),
            font_report: font_sys.report,
        };

        let svg1 = render_page_svg(
            &doc.pages[0],
            &doc.fonts,
            &doc.images,
            &LayoutOptions::default(),
        )
        .expect("svg1");
        let svg2 = render_page_svg(
            &doc.pages[0],
            &doc.fonts,
            &doc.images,
            &LayoutOptions::default(),
        )
        .expect("svg2");

        assert_eq!(svg1, svg2, "SVG output must be byte-identical across runs");
        assert!(svg1.contains("<defs>"));
        assert!(svg1.contains("<use href=\"#p0-f"));

        let html1 = render_document_viewer_html(&doc, &SvgViewerOptions::default()).expect("html1");
        let html2 = render_document_viewer_html(&doc, &SvgViewerOptions::default()).expect("html2");
        assert_eq!(html1, html2);
        assert!(html1.contains("Deterministic SVG Output"));
        assert!(html1.contains("default-src 'none'"));
    }

    fn rect_commands(x: f32, y: f32, w: f32, h: f32) -> Vec<PathCommand> {
        vec![
            PathCommand::MoveTo(Point::new(x, y)),
            PathCommand::LineTo(Point::new(x + w, y)),
            PathCommand::LineTo(Point::new(x + w, y + h)),
            PathCommand::LineTo(Point::new(x, y + h)),
            PathCommand::Close,
        ]
    }

    #[test]
    fn test_viewer_ids_are_unique_across_pages() {
        let mut font_sys = FontSystem::new(&LayoutOptions::default());
        let mut engine = TextLayoutEngine::new();
        let mut pages = Vec::new();

        // Each page: the same glyphs, and a fill clipped to a different half of the page
        for (index, clip_x) in [(0_u32, 0.0_f32), (1, 100.0)] {
            let mut page = LayoutPage::new(index, Size::new(200.0, 200.0));
            let spec = ParagraphSpec {
                spans: vec![SpanSpec {
                    text: "Same text".to_string(),
                    family: "Calibri".to_string(),
                    size: 12.0,
                    ..SpanSpec::default()
                }],
                align: TextAlign::Left,
                line_spacing: LineSpacing::Multiple(1.0),
                max_width: None,
                first_line_indent: 0.0,
                origin: Point::new(10.0, 20.0),
                page_index: index,
                block_index: 0,
            };
            for run in engine.layout_paragraph(&spec, &mut font_sys).runs {
                page.items.push(Item::Glyphs(run));
            }
            page.items.push(Item::Group(GroupItem {
                transform: Affine::IDENTITY,
                clip: Some(ClipPath {
                    commands: rect_commands(clip_x, 0.0, 100.0, 200.0),
                    fill_rule: FillRule::NonZero,
                }),
                opacity: 1.0,
                children: vec![Item::Path(PathItem {
                    commands: rect_commands(0.0, 0.0, 200.0, 200.0),
                    fill: Some(FillPaint::Solid(Rgba::rgb(0, 0, 255))),
                    fill_rule: FillRule::NonZero,
                    stroke: None,
                    transform: Affine::IDENTITY,
                })],
            }));
            pages.push(page);
        }

        let doc = LayoutDocument {
            pages,
            fonts: font_sys.store,
            images: ImageStore::new(),
            font_report: font_sys.report,
        };
        let html = render_document_viewer_html(&doc, &SvgViewerOptions::default()).expect("html");

        let ids: Vec<&str> = html
            .match_indices(" id=\"")
            .map(|(i, m)| {
                let rest = &html[i + m.len()..];
                &rest[..rest.find('"').expect("closing quote")]
            })
            .collect();
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "duplicate element ids in viewer HTML"
        );
        assert!(html.contains(r#"clip-path="url(#p1-clip-0)""#));
        assert!(html.contains(r##"<use href="#p1-f"##));
    }
}
