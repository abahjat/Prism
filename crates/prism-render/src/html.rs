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
        // Check if this is a single-page document with embedded viewer
        if document.pages.len() == 1 && self.has_embedded_viewer(&document.pages[0]) {
            // Render content directly without page wrapper
            document.pages[0]
                .content
                .iter()
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
    fn render_page(&self, document: &Document, page: &prism_core::document::Page, page_num: usize) -> String {
        let content = page
            .content
            .iter()
            .map(|block| self.render_content_block(document, block))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"<div class="page">
        <h2>Page {}</h2>
        {}
    </div>"#,
            page_num, content
        )
    }

    /// Render a table block
    fn render_table(&self, document: &Document, table: &prism_core::document::TableBlock) -> String {
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
        html
    }

    /// Render a content block
    fn render_content_block(&self, document: &Document, block: &ContentBlock) -> String {
        match block {
            ContentBlock::Text(text_block) => {
                // Check if this is embedded PDF data
                if text_block.runs.len() == 1 {
                    let run = &text_block.runs[0];
                    if run.text.starts_with("__PDF_DATA__:") {
                        // Extract PDF base64 data
                        let pdf_data = &run.text[13..];  // Skip "__PDF_DATA__:" prefix
                        return format!(
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
                                    const pdfData = atob('{}');
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
                            </div>"#,
                            pdf_data
                        );
                    }
                }

                // Render each text run with its formatting
                let formatted_text = text_block
                    .runs
                    .iter()
                    .map(|run| self.render_text_run(run))
                    .collect::<Vec<_>>()
                    .join("");

                format!(r#"<div class="text-content">{}</div>"#, formatted_text)
            }
            ContentBlock::Image(image_block) => {
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

                        return format!(
                            r#"<img src="data:{};base64,{}" alt="{}" />"#,
                            html_escape(&img_resource.mime_type),
                            base64_data,
                            html_escape(alt_text)
                        );
                    }
                }

                // Fallback if resource not found
                String::from("<p><em>[Image not found]</em></p>")
            }
            ContentBlock::Table(table_block) => {
                self.render_table(document, table_block)
            }
            ContentBlock::Vector(_) => {
                // TODO: Implement vector rendering
                String::from("<p><em>[Vector rendering not yet implemented]</em></p>")
            }
            ContentBlock::Container(_) => {
                // TODO: Implement container rendering
                String::from("<p><em>[Container rendering not yet implemented]</em></p>")
            }
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
        <h1>{}</h1>
        <p>Document with {} pages</p>
        {}
    </div>
</body>
</html>"#,
            html_escape(title),
            html_escape(title),
            document.page_count(),
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
            options: Default::default(),
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
            options: Default::default(),
            filename: None,
        };

        let result = renderer.render(&document, context).await;
        assert!(result.is_ok());

        let html = String::from_utf8(result.unwrap().to_vec()).unwrap();
        assert!(html.contains("2 pages"));
    }
}
