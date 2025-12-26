// SPDX-License-Identifier: AGPL-3.0-only
//! HTML5 renderer for Prism documents.

use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use bytes::Bytes;
use prism_core::document::{ContentBlock, Document};
use prism_core::error::Result;
use prism_core::format::Format;
use prism_core::render::{RenderContext, RenderFeature, Renderer, RendererMetadata};

/// HTML5 renderer
///
/// Renders documents as responsive, accessible HTML5 with embedded CSS.
#[derive(Debug, Default)]
pub struct HtmlRenderer {
    /// Renderer configuration
    config: HtmlConfig,
}

/// Configuration for HTML rendering
#[derive(Debug, Clone)]
pub struct HtmlConfig {
    /// Whether to embed resources (images, fonts) or link externally
    pub embed_resources: bool,

    /// Whether to include CSS styles
    pub include_styles: bool,

    /// Whether to make the output responsive
    pub responsive: bool,

    /// Custom CSS to inject
    pub custom_css: Option<String>,
}

impl Default for HtmlConfig {
    fn default() -> Self {
        Self {
            embed_resources: true,
            include_styles: true,
            responsive: true,
            custom_css: None,
        }
    }
}

impl HtmlRenderer {
    /// Create a new HTML renderer with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new HTML renderer with custom configuration
    #[must_use]
    pub fn with_config(config: HtmlConfig) -> Self {
        Self { config }
    }

    /// Render a text run with its formatting
    fn render_text_run(&self, run: &prism_core::document::TextRun) -> String {
        let mut html = html_escape(&run.text);
        let style = &run.style;

        // Build inline styles
        let mut styles = Vec::new();

        if let Some(ref font_family) = style.font_family {
            styles.push(format!("font-family: {}", html_escape(font_family)));
        }

        if let Some(font_size) = style.font_size {
            styles.push(format!("font-size: {}pt", font_size));
        }

        if let Some(ref color) = style.color {
            styles.push(format!("color: {}", html_escape(color)));
        }

        if let Some(ref bg_color) = style.background_color {
            styles.push(format!("background-color: {}", html_escape(bg_color)));
        }

        // Apply font weight/style/decoration
        if style.bold {
            html = format!("<strong>{}</strong>", html);
        }

        if style.italic {
            html = format!("<em>{}</em>", html);
        }

        if style.underline {
            html = format!("<u>{}</u>", html);
        }

        if style.strikethrough {
            html = format!("<s>{}</s>", html);
        }

        // Wrap in span with inline styles if needed
        if !styles.is_empty() {
            html = format!(r#"<span style="{}">{}</span>"#, styles.join("; "), html);
        }

        html
    }

    /// Check if content contains embedded special viewers (PDF, single images)
    fn has_embedded_viewer(&self, page: &prism_core::document::Page) -> bool {
        // Check if this is a single-block page with PDF data or single image
        if page.content.len() == 1 {
            match &page.content[0] {
                ContentBlock::Text(text_block) => {
                    // Check for PDF embed marker
                    if text_block.runs.len() == 1 {
                        return text_block.runs[0].text.starts_with("__PDF_DATA__:");
                    }
                }
                ContentBlock::Image(_) => {
                    // Single image doesn't need page wrapper
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Render all pages in the document
    fn render_pages(&self, document: &Document) -> String {
        // Check if this is an email or contact format (no page concept)
        let is_email_format = document
            .metadata
            .custom
            .get("format")
            .and_then(|v| {
                if let prism_core::metadata::MetadataValue::String(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }
            })
            .map(|f| matches!(f, "EML" | "MSG" | "MBOX" | "VCF" | "ICS"))
            .unwrap_or(false);

        // Check if this is a single-page document with embedded viewer
        if is_email_format
            || (document.pages.len() == 1 && self.has_embedded_viewer(&document.pages[0]))
        {
            // Render content directly without page wrapper
            document
                .pages
                .iter()
                .flat_map(|page| &page.content)
                .map(|block| self.render_content_block(document, block))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            // Render with page wrappers for multi-page or regular content
            document
                .pages
                .iter()
                .enumerate()
                .map(|(i, page)| self.render_page(document, page, i + 1))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// Render a single page
    fn render_page(
        &self,
        document: &Document,
        page: &prism_core::document::Page,
        page_num: usize,
    ) -> String {
        // Use page dimensions for the container
        let width = page.dimensions.width;
        let height = page.dimensions.height;

        let mut background_style = String::from("background-color: white;");
        let mut skip_first_block = false;

        // Check if first block is a background image candidate (covers full slide)
        if let Some(ContentBlock::Image(img_block)) = page.content.first() {
            // Check if bounds match page dimensions (allow small epsilon)
            if (img_block.bounds.width - width).abs() < 0.1
                && (img_block.bounds.height - height).abs() < 0.1
                && img_block.bounds.x.abs() < 0.1
                && img_block.bounds.y.abs() < 0.1
            {
                if let Some(img_resource) = document
                    .resources
                    .images
                    .iter()
                    .find(|img| img.id == img_block.resource_id)
                {
                    if let Some(ref data) = img_resource.data {
                        let base64_data = general_purpose::STANDARD.encode(data);
                        // Assumes mime_type is available on ImageResource
                        background_style = format!(
                            "background-image: url('data:{};base64,{}'); background-size: cover; background-position: center;",
                            img_resource.mime_type, base64_data
                        );
                        skip_first_block = true;
                    }
                }
            }
        }

        let content = page
            .content
            .iter()
            .enumerate()
            .filter(|(i, _)| !skip_first_block || *i > 0)
            .map(|(_, block)| self.render_content_block(document, block))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"<div class="page" style="width: {}pt; height: {}pt; position: relative; overflow: hidden; {}">
        <div class="page-number" style="display: none;">Page {}</div>
        {}
    </div>"#,
            width, height, background_style, page_num, content
        )
    }

    /// Render a table block
    fn render_table(
        &self,
        document: &Document,
        table: &prism_core::document::TableBlock,
    ) -> String {
        let mut html = String::from(r#"<table class="data-table">"#);

        // Render table rows
        for row in &table.rows {
            html.push_str("<tr>");

            for cell in &row.cells {
                // Handle col_span and row_span
                let mut attrs = String::new();
                if cell.col_span > 1 {
                    attrs.push_str(&format!(r#" colspan="{}""#, cell.col_span));
                }
                if cell.row_span > 1 {
                    attrs.push_str(&format!(r#" rowspan="{}""#, cell.row_span));
                }

                html.push_str(&format!("<td{}>", attrs));

                // Render cell content
                for content_block in &cell.content {
                    match content_block {
                        ContentBlock::Text(text_block) => {
                            let text = text_block
                                .runs
                                .iter()
                                .map(|run| html_escape(&run.text))
                                .collect::<Vec<_>>()
                                .join("");
                            html.push_str(&text);
                        }
                        _ => {
                            // Recursively render other content types if needed
                            html.push_str(&self.render_content_block(document, content_block));
                        }
                    }
                }

                html.push_str("</td>");
            }

            html.push_str("</tr>");
        }

        html.push_str("</table>");

        // Wrap table in absolute div if it has bounds
        if table.bounds.width > 0.0 && table.bounds.height > 0.0 {
            format!(
                r#"<div style="position: absolute; left: {}pt; top: {}pt; width: {}pt; height: {}pt;">{}</div>"#,
                table.bounds.x, table.bounds.y, table.bounds.width, table.bounds.height, html
            )
        } else {
            html
        }
    }

    /// Render a content block
    fn render_content_block(&self, document: &Document, block: &ContentBlock) -> String {
        match block {
            ContentBlock::Text(text_block) => {
                // Check if this is embedded PDF data
                if text_block.runs.len() == 1
                    && text_block.runs[0].text.starts_with("__PDF_DATA__:")
                {
                    return self.render_pdf_viewer(&text_block.runs[0].text);
                }
                self.render_text_block(text_block)
            }
            ContentBlock::Image(image_block) => self.render_image_block(document, image_block),
            ContentBlock::Table(table_block) => self.render_table(document, table_block),
            ContentBlock::Vector(vector_block) => self.render_vector(document, vector_block),
            ContentBlock::Container(container_block) => {
                self.render_container(document, container_block)
            }
        }
    }

    /// Render embedded PDF viewer
    fn render_pdf_viewer(&self, text: &str) -> String {
        let pdf_data = &text[13..]; // Skip "__PDF_DATA__:" prefix
        format!(
            r#"<div class="pdf-viewer-container">
                <canvas id="pdf-canvas" style="width: 100%; border: 1px solid #ccc;"></canvas>
                <div class="pdf-controls" style="margin-top: 10px; text-align: center;">
                    <button onclick="prevPage()" style="margin: 0 5px;">Previous</button>
                    <span id="page-info">Page <span id="current-page">1</span> of <span id="total-pages">1</span></span>
                    <button onclick="nextPage()" style="margin: 0 5px;">Next</button>
                </div>
                <script src="https://cdnjs.cloudflare.com/ajax/libs/pdf.js/3.11.174/pdf.min.js"></script>
                <script>
                    pdfjsLib.GlobalWorkerOptions.workerSrc = 'https://cdnjs.cloudflare.com/ajax/libs/pdf.js/3.11.174/pdf.worker.min.js';
                    const pdfData = atob('{pdf_data}');
                    const loadingTask = pdfjsLib.getDocument({{data: Uint8Array.from(pdfData, c => c.charCodeAt(0))}});
                    let pdfDoc = null;
                    let pageNum = 1;
                    let rendering = false;

                    loadingTask.promise.then(pdf => {{
                        pdfDoc = pdf;
                        document.getElementById('total-pages').textContent = pdf.numPages;
                        renderPage(pageNum);
                    }});

                    function renderPage(num) {{
                        rendering = true;
                        pdfDoc.getPage(num).then(page => {{
                            const canvas = document.getElementById('pdf-canvas');
                            const ctx = canvas.getContext('2d');
                            const viewport = page.getViewport({{scale: 1.5}});

                            canvas.height = viewport.height;
                            canvas.width = viewport.width;

                            page.render({{
                                canvasContext: ctx,
                                viewport: viewport
                            }}).promise.then(() => {{
                                rendering = false;
                                document.getElementById('current-page').textContent = num;
                            }});
                        }});
                    }}

                    function nextPage() {{
                        if (pageNum >= pdfDoc.numPages || rendering) return;
                        pageNum++;
                        renderPage(pageNum);
                    }}

                    function prevPage() {{
                        if (pageNum <= 1 || rendering) return;
                        pageNum--;
                        renderPage(pageNum);
                    }}
                </script>
            </div>"#
        )
    }

    /// Render a text block
    fn render_text_block(&self, text_block: &prism_core::document::TextBlock) -> String {
        // Render each text run with its formatting
        let formatted_text = text_block
            .runs
            .iter()
            .map(|run| self.render_text_run(run))
            .collect::<Vec<_>>()
            .join("");

        // Determine positioning style
        let pos_style = if text_block.bounds.width > 0.0 && text_block.bounds.height > 0.0 {
            format!(
                "position: absolute; left: {}pt; top: {}pt; width: {}pt; height: {}pt;",
                text_block.bounds.x,
                text_block.bounds.y,
                text_block.bounds.width,
                text_block.bounds.height
            )
        } else {
            String::from("position: relative; margin-bottom: 1em;")
        };

        // Apply rotation if needed
        let transform_style = if text_block.rotation != 0.0 {
            format!(
                "transform: rotate({}deg); transform-origin: center;",
                text_block.rotation
            )
        } else {
            String::new()
        };

        // Apply styles (background, border) from the shape
        let mut shape_styles = Vec::new();
        if let Some(ref bg) = text_block.style.fill_color {
            shape_styles.push(format!("background-color: {};", html_escape(bg)));
        }

        if let Some(ref stroke) = text_block.style.stroke_color {
            shape_styles.push(format!(
                "border: {}pt solid {};",
                text_block.style.stroke_width.unwrap_or(1.0),
                html_escape(stroke)
            ));
        }

        format!(
            r#"<div class="text-content" style="{pos_style} {transform_style} {}">{formatted_text}</div>"#,
            shape_styles.join(" ")
        )
    }

    /// Render an image block
    fn render_image_block(
        &self,
        document: &Document,
        image_block: &prism_core::document::ImageBlock,
    ) -> String {
        let img_tag = {
            // Find the image resource by ID
            if let Some(img_resource) = document
                .resources
                .images
                .iter()
                .find(|img| img.id == image_block.resource_id)
            {
                // Base64 encode the image data if available
                if let Some(ref data) = img_resource.data {
                    let base64_data = general_purpose::STANDARD.encode(data);
                    let alt_text = image_block.alt_text.as_deref().unwrap_or("Image");

                    format!(
                        r#"<img src="data:{};base64,{base64_data}" alt="{}" style="width: 100%; height: 100%;" />"#,
                        html_escape(&img_resource.mime_type),
                        html_escape(alt_text)
                    )
                } else {
                    String::from("<p><em>[Image data missing]</em></p>")
                }
            } else {
                // Fallback if resource not found
                String::from("<p><em>[Image not found]</em></p>")
            }
        };

        // Position wrapper
        if image_block.bounds.width > 0.0 && image_block.bounds.height > 0.0 {
            format!(
                r#"<div class="image-container" style="position: absolute; left: {}pt; top: {}pt; width: {}pt; height: {}pt;">{img_tag}</div>"#,
                image_block.bounds.x,
                image_block.bounds.y,
                image_block.bounds.width,
                image_block.bounds.height
            )
        } else {
            format!(r#"<div class="image-container">{img_tag}</div>"#)
        }
    }

    /// Render a vector block
    fn render_vector(
        &self,
        _document: &Document,
        vector: &prism_core::document::VectorBlock,
    ) -> String {
        let mut paths_svg = String::new();
        for path in &vector.paths {
            let mut d = String::new();
            for cmd in &path.commands {
                use prism_core::document::PathCommand::*;
                match cmd {
                    MoveTo(p) => d.push_str(&format!("M {} {} ", p.x, p.y)),
                    LineTo(p) => d.push_str(&format!("L {} {} ", p.x, p.y)),
                    CurveTo { cp1, cp2, end } => d.push_str(&format!(
                        "C {} {} {} {} {} {} ",
                        cp1.x, cp1.y, cp2.x, cp2.y, end.x, end.y
                    )),
                    QuadTo { cp, end } => {
                        d.push_str(&format!("Q {} {} {} {} ", cp.x, cp.y, end.x, end.y))
                    }
                    Close => d.push_str("Z "),
                }
            }

            let fill = path.fill.as_deref().unwrap_or("none");
            let stroke = path.stroke.as_deref().unwrap_or("none");
            let stroke_width = path.stroke_width.unwrap_or(0.0);

            paths_svg.push_str(&format!(
                r#"<path d="{}" fill="{}" stroke="{}" stroke-width="{}" />"#,
                d.trim(),
                html_escape(fill),
                html_escape(stroke),
                stroke_width
            ));
        }

        // Wrap in SVG
        let svg = format!(
            r#"<svg viewBox="0 0 {} {}" width="100%" height="100%" preserveAspectRatio="none">{}</svg>"#,
            vector.bounds.width, vector.bounds.height, paths_svg
        );

        // Position wrapper
        if vector.bounds.width > 0.0 && vector.bounds.height > 0.0 {
            format!(
                r#"<div class="vector-block" style="position: absolute; left: {}pt; top: {}pt; width: {}pt; height: {}pt;">{}</div>"#,
                vector.bounds.x, vector.bounds.y, vector.bounds.width, vector.bounds.height, svg
            )
        } else {
            format!(r#"<div class="vector-block">{}</div>"#, svg)
        }
    }

    /// Render a container block
    fn render_container(
        &self,
        document: &Document,
        container: &prism_core::document::ContainerBlock,
    ) -> String {
        let content = container
            .children
            .iter()
            .map(|b| self.render_content_block(document, b))
            .collect::<Vec<_>>()
            .join("\n");

        if container.bounds.width > 0.0 && container.bounds.height > 0.0 {
            format!(
                r#"<div class="container-block" style="position: absolute; left: {}pt; top: {}pt; width: {}pt; height: {}pt;">{}</div>"#,
                container.bounds.x,
                container.bounds.y,
                container.bounds.width,
                container.bounds.height,
                content
            )
        } else {
            format!(r#"<div class="container-block">{}</div>"#, content)
        }
    }
}

/// Escape HTML special characters to prevent XSS
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[async_trait]
impl Renderer for HtmlRenderer {
    fn output_format(&self) -> Format {
        Format {
            mime_type: "text/html".to_string(),
            extension: "html".to_string(),
            family: prism_core::format::FormatFamily::Text,
            name: "HTML5".to_string(),
            is_container: false,
        }
    }

    async fn render(&self, document: &Document, _context: RenderContext) -> Result<Bytes> {
        let title = document
            .metadata
            .title
            .as_deref()
            .unwrap_or("Untitled Document");

        // Check if this is a single-page document with embedded viewer
        let has_embedded =
            document.pages.len() == 1 && self.has_embedded_viewer(&document.pages[0]);

        let html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{}</title>
    <style>
        body {{
            font-family: Arial, sans-serif;
            margin: 0;
            padding: 2rem;
            background-color: #f5f5f5;
        }}
        .container {{
            max-width: 1200px;
            margin: 0 auto;
            background-color: white;
            padding: 2rem;
            box-shadow: 0 2px 8px rgba(0,0,0,0.1);
        }}
        h1 {{
            color: #333;
            margin-top: 0;
        }}
        .page {{
            margin-bottom: 2rem;
            padding: 1rem;
            text-align: center;
        }}
        .page img {{
            max-width: 100%;
            height: auto;
            border: 1px solid #ddd;
            border-radius: 4px;
            box-shadow: 0 1px 3px rgba(0,0,0,0.1);
        }}
        .text-content {{
            text-align: left;
            white-space: pre-wrap;
            word-wrap: break-word;
            overflow-wrap: break-word;
            font-family: monospace;
            background-color: #f8f8f8;
            padding: 1rem;
            border-radius: 4px;
            line-height: 1.5;
            max-width: 100%;
        }}
        .data-table {{
            width: 100%;
            border-collapse: collapse;
            margin: 1rem 0;
            font-size: 0.9rem;
            box-shadow: 0 2px 5px rgba(0,0,0,0.1);
        }}
        .data-table td {{
            border: 1px solid #ddd;
            padding: 8px 12px;
            text-align: left;
            vertical-align: top;
        }}
        .data-table tr:nth-child(even) {{
            background-color: #f9f9f9;
        }}
        .data-table tr:hover {{
            background-color: #f5f5f5;
        }}
    </style>
</head>
<body>
    <div class="container">
        {}
        {}
    </div>
</body>
</html>"#,
            html_escape(title),
            // No header - removed filename and page count
            String::new(),
            self.render_pages(document)
        );

        Ok(Bytes::from(html))
    }

    fn metadata(&self) -> RendererMetadata {
        RendererMetadata {
            name: "HTML5 Renderer".to_string(),
            version: crate::VERSION.to_string(),
            features: vec![
                RenderFeature::TextRendering,
                RenderFeature::ImageRendering,
                RenderFeature::TableRendering,
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::document::{Dimensions, Page};
    use prism_core::metadata::Metadata;

    #[tokio::test]
    async fn test_html_renderer() {
        let renderer = HtmlRenderer::new();
        assert_eq!(renderer.output_format().mime_type, "text/html");
    }

    #[tokio::test]
    async fn test_render_empty_document() {
        let renderer = HtmlRenderer::new();
        let document = Document::builder()
            .metadata(Metadata::builder().title("Test").build())
            .build();

        let context = RenderContext {
            options: prism_core::render::RenderOptions::default(),
            filename: None,
        };

        let result = renderer.render(&document, context).await;
        assert!(result.is_ok());

        let html = String::from_utf8(result.unwrap().to_vec()).unwrap();
        assert!(html.contains("Test"));
        assert!(html.contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn test_render_with_pages() {
        let renderer = HtmlRenderer::new();

        let page1 = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content: vec![],
            metadata: Default::default(),
            annotations: vec![],
        };

        let page2 = Page {
            number: 2,
            dimensions: Dimensions::LETTER,
            content: vec![],
            metadata: Default::default(),
            annotations: vec![],
        };

        let document = Document::builder()
            .metadata(Metadata::builder().title("Multi-page").build())
            .page(page1)
            .page(page2)
            .build();

        let context = RenderContext {
            options: prism_core::render::RenderOptions::default(),
            filename: None,
        };

        let result = renderer.render(&document, context).await;
        assert!(result.is_ok());

        let html = String::from_utf8(result.unwrap().to_vec()).unwrap();
        assert!(html.contains("Page 1"));
        assert!(html.contains("Page 2"));
    }
}
