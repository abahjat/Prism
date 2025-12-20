//! HTML5 renderer for Prism documents.

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::document::Document;
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
        // TODO: Implement actual HTML rendering
        // This is a placeholder implementation

        let html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{}</title>
    <style>
        body {{ font-family: Arial, sans-serif; margin: 2rem; }}
        .page {{ margin-bottom: 2rem; border: 1px solid #ccc; padding: 1rem; }}
    </style>
</head>
<body>
    <h1>{}</h1>
    <p>Document with {} pages</p>
    {}
</body>
</html>"#,
            document
                .metadata
                .title
                .as_deref()
                .unwrap_or("Untitled Document"),
            document
                .metadata
                .title
                .as_deref()
                .unwrap_or("Untitled Document"),
            document.page_count(),
            document
                .pages
                .iter()
                .enumerate()
                .map(|(i, page)| {
                    format!(
                        r#"<div class="page">
        <h2>Page {}</h2>
        <pre>{}</pre>
    </div>"#,
                        i + 1,
                        page.extract_text()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
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
        let document = Document::builder()
            .metadata(Metadata::builder().title("Multi-page").build())
            .page(Page::new(1, Dimensions::LETTER))
            .page(Page::new(2, Dimensions::LETTER))
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
