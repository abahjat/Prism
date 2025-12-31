use prism_core::parser::{ParseContext, Parser};
use prism_parsers::DocxParser;
use std::path::PathBuf;
use tokio::fs;

#[tokio::test]
async fn test_repro_docx_issues() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../test-files/testInlineImage.docx");

    println!("Reading file: {:?}", path);
    let bytes = fs::read(&path).await.expect("Failed to read test file");
    let bytes = bytes::Bytes::from(bytes);

    let parser = DocxParser::new();
    let context = ParseContext {
        format: prism_core::format::Format::docx(),
        filename: Some("testInlineImage.docx".to_string()),
        size: bytes.len(),
        options: prism_core::parser::ParseOptions::default(),
    };

    let document = parser.parse(bytes, context).await.expect("Failed to parse");

    println!("Pages: {}", document.pages.len());

    for page in document.pages {
        println!("Page {}", page.number);
        for block in page.content {
            match block {
                prism_core::document::ContentBlock::Text(text_block) => {
                    for run in text_block.runs {
                        println!("Text: '{}'", run.text);
                        let style = run.style;
                        if let Some(color) = style.color {
                            println!("  Color: {}", color);
                        } else {
                            println!("  No color");
                        }
                        println!("  Bold: {}", style.bold);
                    }
                }
                prism_core::document::ContentBlock::Image(image_block) => {
                    println!(
                        "Image Block found. Resource ID: {}",
                        image_block.resource_id
                    );
                    if let Some(resource) = document
                        .resources
                        .images
                        .iter()
                        .find(|r| r.id == image_block.resource_id)
                    {
                        println!("  Resource found:");
                        println!("    MIME: {}", resource.mime_type);
                        println!("    Dimensions: {}x{}", resource.width, resource.height);
                        if let Some(data) = &resource.data {
                            println!("    Data size: {} bytes", data.len());
                        } else {
                            println!("    No image data");
                        }
                    } else {
                        println!("  Resource NOT found in document.resources");
                    }
                }
                prism_core::document::ContentBlock::Table(_) => {
                    println!("Table found");
                }
                _ => {
                    println!("Other block type");
                }
            }
        }
    }
}
