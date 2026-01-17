// SPDX-License-Identifier: AGPL-3.0-only
//! Code syntax highlighting parser

use async_trait::async_trait;
use bytes::Bytes;
use prism_core::{
    document::{ContentBlock, Dimensions, Document, Page, Rect, TextBlock, TextRun, TextStyle},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Shared `SyntaxSet` and `ThemeSet` to avoid reloading
static SYNTAX_SET: std::sync::LazyLock<SyntaxSet> =
    std::sync::LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: std::sync::LazyLock<ThemeSet> = std::sync::LazyLock::new(ThemeSet::load_defaults);

/// Parser for source code files with syntax highlighting
pub struct CodeParser {
    format: Format,
}

impl CodeParser {
    /// Create a new code parser for the given format
    #[must_use]
    pub fn new(format: Format) -> Self {
        Self { format }
    }
}

#[async_trait]
impl Parser for CodeParser {
    fn format(&self) -> Format {
        self.format.clone()
    }

    fn can_parse(&self, _data: &[u8]) -> bool {
        // Can parse any text if we are assigned to it
        // Format detection usually handles assignment
        true
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let text = String::from_utf8_lossy(&data).to_string();

        // Determine syntax
        // Try by extension first
        let extension = self.format.extension.as_str();

        let syntax = if extension.is_empty() {
            None
        } else {
            SYNTAX_SET.find_syntax_by_extension(extension)
        };

        // Fallback to plain text if not found, or name
        let syntax = syntax
            .or_else(|| SYNTAX_SET.find_syntax_by_name(&self.format.name))
            .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

        // Use a default theme
        let theme = &THEME_SET.themes["base16-ocean.dark"]; // Using a dark theme

        let mut highlighter = HighlightLines::new(syntax, theme);

        let mut runs = Vec::new();

        // Highlighting works line by line
        for line in text.lines() {
            // syntect expects newlines for some syntaxes, lines() strips them
            // We'll append newline manually to runs if needed, or simple separate blocks?
            // Usually one big block is fine.
            let ranges = highlighter
                .highlight_line(line, &SYNTAX_SET)
                .map_err(|e| Error::ParseError(format!("Highlighting failed: {e}")))?;

            for (style, content) in ranges {
                let mut text_style = TextStyle::default();

                // Convert Color to hex string
                let fg = style.foreground;
                text_style.color = Some(format!("#{:02x}{:02x}{:02x}", fg.r, fg.g, fg.b));

                // We ignore background for individual runs usually to avoid blocky look,
                // or we could set it. Let's strictly do foreground for now.
                // text_style.background_color = ...

                if style
                    .font_style
                    .contains(syntect::highlighting::FontStyle::BOLD)
                {
                    text_style.bold = true;
                }
                if style
                    .font_style
                    .contains(syntect::highlighting::FontStyle::ITALIC)
                {
                    text_style.italic = true;
                }
                if style
                    .font_style
                    .contains(syntect::highlighting::FontStyle::UNDERLINE)
                {
                    text_style.underline = true;
                }

                runs.push(TextRun::with_style(content.to_string(), text_style));
            }
            // Add newline run
            runs.push(TextRun::new("\n"));
        }

        let mut block = TextBlock::new(Rect::new(20.0, 20.0, 550.0, 750.0));
        block.runs = runs;
        // Set background of whole block to theme bg?
        if let Some(bg) = theme.settings.background {
            block.style.fill_color = Some(format!("#{:02x}{:02x}{:02x}", bg.r, bg.g, bg.b));
        }

        // We create a single page document
        let mut page = Page::new(1, Dimensions::LETTER);

        // Add content
        page.add_content(ContentBlock::Text(block));

        let mut metadata = Metadata::default();
        if let Some(ref filename) = context.filename {
            metadata.title = Some(filename.clone());
        }
        metadata.add_custom("format", self.format.name.as_str());
        metadata.add_custom("language", syntax.name.as_str());

        let mut doc = Document::new();
        doc.pages.push(page);
        doc.metadata = metadata;

        Ok(doc)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: format!("{} Parser", self.format.name),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![ParserFeature::TextExtraction], // And Styling
            requires_sandbox: false,
        }
    }
}
