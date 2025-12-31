use prism_core::parser::{ParseContext, Parser};
use prism_parsers::email::MsgParser;
use std::path::PathBuf;
use tokio::fs;

#[tokio::test]
async fn test_msg_detection_and_parsing() {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../test-files/Regular.msg");

    println!("Reading file: {:?}", path);
    let bytes = fs::read(&path).await.expect("Failed to read test file");

    // 1. Test Detection
    println!("--- Testing Detection ---");
    let detection = prism_core::format::detect_format(&bytes, Some("Regular.msg"));
    if let Some(d) = &detection {
        println!(
            "Detected format: {} ({})",
            d.format.name, d.format.mime_type
        );
        println!("Method: {:?}", d.method);
    } else {
        println!("Format NOT detected");
    }

    // 2. Test Parsing and Attachments
    println!("--- Testing Parsing ---");
    let parser = MsgParser::new();
    let context = ParseContext {
        format: prism_core::format::Format::msg(),
        filename: Some("Regular.msg".to_string()),
        size: bytes.len(),
        options: prism_core::parser::ParseOptions::default(),
    };

    let document = parser
        .parse(bytes::Bytes::from(bytes), context)
        .await
        .expect("Failed to parse MSG");

    println!("Metadata Title: {:?}", document.metadata.title);
    println!("Attachment Count: {}", document.attachments.len());

    for (i, attach) in document.attachments.iter().enumerate() {
        println!("Attachment {}: {}", i, attach.filename);
        println!("  Mime: {:?}", attach.mime_type);
        println!("  Size: {} bytes", attach.data.len());
    }
}
