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

    /// Render all pages in the document
    fn render_pages(&self, document: &Document) -> String {
        document
            .pages
            .iter()
            .enumerate()
            .map(|(i, page)| self.render_page(document, page, i + 1))
            .collect::<Vec<_>>()
            .join("\n")
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
                let text = text_block
                    .runs
                    .iter()
                    .map(|run| &run.text)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("");

                format!(
                    r#"<div class="text-content">{}</div>"#,
                    html_escape(&text)
                )
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
