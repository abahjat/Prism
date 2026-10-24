// SPDX-License-Identifier: AGPL-3.0-only
//! Unified document detection, parsing, and paginated layout pipeline (`prism_parsers::pipeline`).
//!
//! Centralizes the format-dispatch pipeline shared across `prism-bindings`, `prism-sandbox`,
//! and `prism-production` so every host entrypoint uses the same native layout engines
//! (`prism-word`, `prism-ppt`, `prism-excel`, `prism-layout`) and a single shared
//! [`ParserRegistry`] instance.

use crate::registry::ParserRegistry;
use bytes::Bytes;
use prism_core::document::Document;
use prism_core::error::{Error, Result};
use prism_core::format::{Format, detect_format};
use prism_core::metadata::Metadata;
use prism_core::parser::{ParseContext, ParseOptions};
use prism_core::source::DocumentSource;
use prism_excel::{Workbook, layout_workbook, parse_xls, parse_xlsx};
use prism_layout::{
    LayoutDocument, LayoutOptions, interpret_pdf_bytes, interpret_pdf_pages,
    layout_document_paginated, layout_document_paginated_range,
};
use prism_ppt::{Presentation, layout_presentation, parse_ppt, parse_pptx};
use prism_word::{
    WordDocument, layout_word_document, layout_word_document_range, parse_doc, parse_docx,
};
use std::ops::Range;
use std::path::Path;
use std::sync::LazyLock;

static DEFAULT_REGISTRY: LazyLock<ParserRegistry> =
    LazyLock::new(ParserRegistry::with_default_parsers);

/// Return a reference to the shared default [`ParserRegistry`].
#[must_use]
pub fn default_registry() -> &'static ParserRegistry {
    &DEFAULT_REGISTRY
}

/// Result of opening and laying out a document via [`open_and_layout`].
pub struct OpenedDocument {
    /// Detected document format.
    pub format: Format,
    /// Positioned and paginated layout document.
    pub layout: LayoutDocument,
    /// Document metadata (title, author, custom properties).
    pub metadata: Metadata,
    /// Native Excel workbook model when the input is `.xlsx` or `.xls`.
    pub workbook: Option<Workbook>,
    /// Native `PowerPoint` presentation model when the input is `.pptx` or `.ppt`.
    pub presentation: Option<Presentation>,
    /// Native Word document model when the input is `.docx` or `.doc`.
    pub word_doc: Option<WordDocument>,
}

/// Detect the format of `data`, parse it with the shared [`ParserRegistry`], and return
/// the detected [`Format`] and Unified Document Model [`Document`].
///
/// # Errors
///
/// Returns an [`Error`] if format detection fails, no parser is registered for the format,
/// or parsing fails.
pub fn parse_udm_bytes(
    data: &[u8],
    filename: Option<&str>,
    options: ParseOptions,
) -> Result<(Format, Document)> {
    let detection = detect_format(data, filename)
        .ok_or_else(|| Error::DetectionFailed("Unrecognized document format".to_string()))?;
    let format = detection.format;
    let parser = default_registry()
        .get_parser(&format)
        .ok_or_else(|| Error::UnsupportedFormat(format.name.clone()))?;

    let context = ParseContext {
        format: format.clone(),
        filename: filename.map(str::to_string),
        size: data.len(),
        options,
    };
    let bytes = Bytes::copy_from_slice(data);

    let udm = if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|s| {
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| Error::Internal(format!("tokio runtime: {e}")))?;
                rt.block_on(parser.parse(bytes, context))
            })
            .join()
            .map_err(|_| Error::Internal("Parser thread panicked".to_string()))?
        })?
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Internal(format!("tokio runtime: {e}")))?;
        rt.block_on(parser.parse(bytes, context))?
    };

    Ok((format, udm))
}

/// Detect the format of `data`, parse it using the highest-fidelity native engine available
/// (`hayro` for PDF, `prism-word` for DOCX/DOC, `prism-ppt` for PPTX/PPT, `prism-excel` for
/// XLSX/XLS, and `layout_document_paginated` for email, web, image, archive, CAD, and text
/// formats), and return an [`OpenedDocument`].
///
/// # Errors
///
/// Returns an [`Error`] if format detection fails, the document is encrypted or malformed,
/// or resource limits in `options` are exceeded.
#[allow(clippy::too_many_lines)]
pub fn open_and_layout(
    data: &[u8],
    filename: Option<&str>,
    options: &LayoutOptions,
) -> Result<OpenedDocument> {
    let detection = detect_format(data, filename)
        .ok_or_else(|| Error::DetectionFailed("Unrecognized document format".to_string()))?;
    let format = detection.format;
    let ext = format.extension.to_ascii_lowercase();
    let max_stream = options.max_decompressed_stream_bytes;

    match ext.as_str() {
        "pdf" => {
            let layout = interpret_pdf_bytes(data, options)?;
            Ok(OpenedDocument {
                format,
                layout,
                metadata: crate::pdf::PdfParser::read_metadata(data),
                workbook: None,
                presentation: None,
                word_doc: None,
            })
        }
        "docx" => {
            let word_doc = parse_docx(data, filename, max_stream)?;
            let layout = layout_word_document(&word_doc, options)?;
            let metadata = Metadata {
                title: word_doc.title.clone(),
                author: word_doc.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: None,
                presentation: None,
                word_doc: Some(word_doc),
            })
        }
        "doc" => {
            let word_doc = parse_doc(data, max_stream)?;
            let layout = layout_word_document(&word_doc, options)?;
            let metadata = Metadata {
                title: word_doc.title.clone(),
                author: word_doc.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: None,
                presentation: None,
                word_doc: Some(word_doc),
            })
        }
        "pptx" | "ppsx" | "potx" => {
            let pres = parse_pptx(data, max_stream)?;
            let layout = layout_presentation(&pres, options)?;
            let metadata = Metadata {
                title: pres.title.clone(),
                author: pres.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: None,
                presentation: Some(pres),
                word_doc: None,
            })
        }
        "ppt" | "pps" | "pot" => {
            let pres = parse_ppt(data, max_stream)?;
            let layout = layout_presentation(&pres, options)?;
            let metadata = Metadata {
                title: pres.title.clone(),
                author: pres.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: None,
                presentation: Some(pres),
                word_doc: None,
            })
        }
        "xlsx" => {
            let wb = parse_xlsx(data, max_stream)?;
            let layout = layout_workbook(&wb, options)?;
            let metadata = Metadata {
                title: wb.title.clone(),
                author: wb.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: Some(wb),
                presentation: None,
                word_doc: None,
            })
        }
        "xls" => {
            let wb = parse_xls(data, max_stream)?;
            let layout = layout_workbook(&wb, options)?;
            let metadata = Metadata {
                title: wb.title.clone(),
                author: wb.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: Some(wb),
                presentation: None,
                word_doc: None,
            })
        }
        _ => {
            let parse_opts = ParseOptions {
                max_memory: Some(max_stream),
                ..ParseOptions::default()
            };
            let (_, udm) = parse_udm_bytes(data, filename, parse_opts)?;
            let layout = layout_document_paginated(&udm, options)?;
            Ok(OpenedDocument {
                format,
                layout,
                metadata: udm.metadata,
                workbook: None,
                presentation: None,
                word_doc: None,
            })
        }
    }
}

/// Detect the format of a [`DocumentSource`] via bounded prefix inspection and lay out the document.
///
/// # Errors
///
/// Returns an [`Error`] if format detection fails, the source exceeds the configured byte cap,
/// or parsing/layout fails.
pub fn open_and_layout_source(
    source: &DocumentSource,
    options: &LayoutOptions,
) -> Result<OpenedDocument> {
    let _ = source
        .detect_format()?
        .ok_or_else(|| Error::DetectionFailed("Unrecognized document format".to_string()))?;
    let max_bytes = options
        .max_decompressed_stream_bytes
        .saturating_mul(2)
        .max(options.max_decompressed_stream_bytes);
    let data = source.to_bytes_bounded(max_bytes)?;
    open_and_layout(&data, source.filename(), options)
}

/// Open a document directly from a filesystem path (`Read + Seek` streaming source) and lay it out.
///
/// # Errors
///
/// Returns an [`Error`] if the file cannot be opened, format detection fails, or parsing/layout fails.
pub fn open_and_layout_path(
    path: impl AsRef<Path>,
    options: &LayoutOptions,
) -> Result<OpenedDocument> {
    let source = DocumentSource::from_path(path)?;
    open_and_layout_source(&source, options)
}

/// Detect the format of `data`, parse it, and lay out only the requested zero-based `page_range`.
///
/// This avoids constructing `Page` trees outside `page_range` for PDF (`interpret_pdf_pages`),
/// Word (`layout_word_document_range`), and UDM flow formats (`layout_document_paginated_range`).
///
/// # Errors
///
/// Returns an [`Error`] if format detection fails, the document is encrypted or malformed,
/// or resource limits in `options` are exceeded.
#[allow(clippy::too_many_lines)]
pub fn open_and_layout_page_range(
    data: &[u8],
    filename: Option<&str>,
    options: &LayoutOptions,
    page_range: Range<usize>,
) -> Result<OpenedDocument> {
    let detection = detect_format(data, filename)
        .ok_or_else(|| Error::DetectionFailed("Unrecognized document format".to_string()))?;
    let format = detection.format;
    let ext = format.extension.to_ascii_lowercase();
    let max_stream = options.max_decompressed_stream_bytes;

    match ext.as_str() {
        "pdf" => {
            // No document has more than `max_pages` pages, so a caller's open-ended range
            // (`0..usize::MAX`) is clamped before it is turned into a list of indices.
            let end = page_range.end.min(options.max_pages);
            let page_indices: Vec<u32> = (page_range.start.min(end)..end)
                .filter_map(|idx| u32::try_from(idx).ok())
                .collect();
            let layout = interpret_pdf_pages(data, options, Some(&page_indices))?;
            Ok(OpenedDocument {
                format,
                layout,
                metadata: crate::pdf::PdfParser::read_metadata(data),
                workbook: None,
                presentation: None,
                word_doc: None,
            })
        }
        "docx" => {
            let word_doc = parse_docx(data, filename, max_stream)?;
            let layout = layout_word_document_range(&word_doc, options, page_range)?;
            let metadata = Metadata {
                title: word_doc.title.clone(),
                author: word_doc.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: None,
                presentation: None,
                word_doc: Some(word_doc),
            })
        }
        "doc" => {
            let word_doc = parse_doc(data, max_stream)?;
            let layout = layout_word_document_range(&word_doc, options, page_range)?;
            let metadata = Metadata {
                title: word_doc.title.clone(),
                author: word_doc.author.clone(),
                ..Metadata::default()
            };
            Ok(OpenedDocument {
                format,
                layout,
                metadata,
                workbook: None,
                presentation: None,
                word_doc: Some(word_doc),
            })
        }
        "pptx" | "ppsx" | "potx" | "ppt" | "pps" | "pot" | "xlsx" | "xls" => {
            let mut opened = open_and_layout(data, filename, options)?;
            let total = opened.layout.pages.len();
            let start = page_range.start.min(total);
            let end = page_range.end.min(total);
            opened.layout.pages = if start < end {
                opened.layout.pages.drain(start..end).collect()
            } else {
                Vec::new()
            };
            Ok(opened)
        }
        _ => {
            let parse_opts = ParseOptions {
                max_memory: Some(max_stream),
                ..ParseOptions::default()
            };
            let (_, udm) = parse_udm_bytes(data, filename, parse_opts)?;
            let layout = layout_document_paginated_range(&udm, options, page_range)?;
            Ok(OpenedDocument {
                format,
                layout,
                metadata: udm.metadata,
                workbook: None,
                presentation: None,
                word_doc: None,
            })
        }
    }
}

/// Detect the format of a [`DocumentSource`] and lay out only the requested zero-based `page_range`.
///
/// # Errors
///
/// Returns an [`Error`] if format detection fails, the source exceeds the configured byte cap,
/// or parsing/layout fails.
pub fn open_and_layout_source_page_range(
    source: &DocumentSource,
    options: &LayoutOptions,
    page_range: Range<usize>,
) -> Result<OpenedDocument> {
    let _ = source
        .detect_format()?
        .ok_or_else(|| Error::DetectionFailed("Unrecognized document format".to_string()))?;
    let max_bytes = options
        .max_decompressed_stream_bytes
        .saturating_mul(2)
        .max(options.max_decompressed_stream_bytes);
    let data = source.to_bytes_bounded(max_bytes)?;
    open_and_layout_page_range(&data, source.filename(), options, page_range)
}
