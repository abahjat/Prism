// SPDX-License-Identifier: AGPL-3.0-only
//! PDF document parser
//!
//! Keeps the original PDF for native rendering (see `prism-render`) and
//! extracts its text and document information with lopdf, including recursive
//! traversal of `/Subtype /Form` `XObjects` invoked via `"Do"`.

#![allow(clippy::doc_markdown)]

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};
use lopdf::{Dictionary, Document as LopdfDocument, Encoding, Object, ObjectId, content::Content};
use prism_core::{
    document::{ContentBlock, Dimensions, Document, EmbeddedBlock, Page},
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::collections::BTreeMap;
use tracing::{debug, info, warn};

/// Graphics and text state tracked while walking a PDF page content stream
#[derive(Clone)]
struct PdfTextState {
    ctm: [f64; 6],
    font_name: Option<Vec<u8>>,
    font_size: f64,
    char_space: f64,
    word_space: f64,
    horiz_scale: f64,
    leading: f64,
}

impl Default for PdfTextState {
    fn default() -> Self {
        Self {
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            font_name: None,
            font_size: 12.0,
            char_space: 0.0,
            word_space: 0.0,
            horiz_scale: 1.0,
            leading: 0.0,
        }
    }
}

/// Multiply two 3x3 affine transformation matrices `[a, b, c, d, e, f]`
fn multiply_matrix(m1: [f64; 6], m2: [f64; 6]) -> [f64; 6] {
    [
        m1[0] * m2[0] + m1[1] * m2[2],
        m1[0] * m2[1] + m1[1] * m2[3],
        m1[2] * m2[0] + m1[3] * m2[2],
        m1[2] * m2[1] + m1[3] * m2[3],
        m1[4] * m2[0] + m1[5] * m2[2] + m2[4],
        m1[4] * m2[1] + m1[5] * m2[3] + m2[5],
    ]
}

/// Convert a PDF numeric `Object` (`Integer` or `Real`) to `f64`
fn obj_to_f64(obj: &Object) -> Option<f64> {
    match *obj {
        #[allow(clippy::cast_precision_loss)]
        Object::Integer(i) => Some(i as f64),
        Object::Real(f) => Some(f64::from(f)),
        _ => None,
    }
}

/// Estimate the horizontal advance of `text` in PDF text space units
fn estimate_text_advance(text: &str, state: &PdfTextState) -> f64 {
    let mut advance = 0.0;
    for ch in text.chars() {
        let glyph_em = match ch {
            ' ' | 'i' | 'l' | 'I' | '!' | '|' | '.' | ',' | ':' | ';' | '\'' => 0.28,
            'f' | 'j' | 'r' | 't' | '(' | ')' | '[' | ']' | '-' => 0.35,
            'm' | 'w' | 'M' | 'W' => 0.75,
            c if c.is_ascii_uppercase() => 0.60,
            _ => 0.50,
        };
        let word_extra = if ch == ' ' { state.word_space } else { 0.0 };
        advance += (glyph_em * state.font_size + state.char_space + word_extra) * state.horiz_scale;
    }
    advance
}

/// Decode a raw PDF string operand using the active font's encoding when available
fn decode_pdf_bytes(
    bytes: &[u8],
    font_name: Option<&[u8]>,
    local_encodings: Option<&BTreeMap<Vec<u8>, Encoding<'_>>>,
    page_encodings: &BTreeMap<Vec<u8>, Encoding<'_>>,
) -> String {
    let enc = font_name.and_then(|name| {
        local_encodings
            .and_then(|m| m.get(name))
            .or_else(|| page_encodings.get(name))
    });
    if let Some(enc) = enc
        && let Ok(decoded) = LopdfDocument::decode_text(enc, bytes)
    {
        return decoded;
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// PDF document parser
#[derive(Debug, Clone)]
pub struct PdfParser;

impl PdfParser {
    /// Create a new PDF parser
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Read only the document information dictionary (title, author, dates, ...) of `data`,
    /// for callers that lay the PDF out with another engine. A PDF that cannot be loaded
    /// yields default metadata.
    #[must_use]
    pub fn read_metadata(data: &[u8]) -> Metadata {
        let mut metadata = Metadata::default();
        metadata.add_custom("format", "PDF");
        if let Ok(mut pdf) = LopdfDocument::load_mem(data) {
            let was_encrypted = pdf.is_encrypted();
            if was_encrypted {
                let _ = pdf.decrypt("");
            }
            Self::extract_metadata(&pdf, &mut metadata);
            metadata.add_custom("encrypted", was_encrypted);
        }
        metadata
    }

    /// Read the document information dictionary into `metadata`
    fn extract_metadata(pdf: &LopdfDocument, metadata: &mut Metadata) {
        let Some(info) = pdf
            .trailer
            .get(b"Info")
            .ok()
            .and_then(|obj| match obj.as_reference() {
                Ok(id) => pdf.get_dictionary(id).ok(),
                Err(_) => obj.as_dict().ok(),
            })
        else {
            return;
        };

        metadata.title = info_string(info, b"Title");
        metadata.author = info_string(info, b"Author");
        metadata.subject = info_string(info, b"Subject");
        metadata.creator = info_string(info, b"Creator");
        metadata.producer = info_string(info, b"Producer");
        if let Some(keywords) = info_string(info, b"Keywords") {
            metadata.keywords = keywords
                .split([',', ';'])
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(raw) = info_string(info, b"CreationDate") {
            metadata.created = parse_pdf_date(&raw);
            metadata.add_custom("pdf_creation_date", raw);
        }
        if let Some(raw) = info_string(info, b"ModDate") {
            metadata.modified = parse_pdf_date(&raw);
            metadata.add_custom("pdf_mod_date", raw);
        }
    }

    /// Extract the text of every page, pages separated by blank lines.
    ///
    /// Pages whose content cannot be decoded are skipped rather than failing
    /// the whole document.
    fn extract_text(pdf: &LopdfDocument, stream_limit: usize) -> String {
        pdf.get_pages()
            .into_values()
            .filter_map(|page_id| Self::extract_page_text(pdf, page_id, stream_limit))
            .map(|page| page.trim().to_string())
            .filter(|page| !page.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Extract text from a single page using position-aware content stream traversal,
    /// including recursive traversal of `/Subtype /Form` XObjects invoked by `"Do"`.
    fn extract_page_text(
        pdf: &LopdfDocument,
        page_id: ObjectId,
        stream_limit: usize,
    ) -> Option<String> {
        let content_bytes = pdf
            .get_page_content_with_limit(page_id, stream_limit)
            .ok()?;
        let content = Content::decode(&content_bytes).ok()?;
        let fonts = pdf.get_page_fonts(page_id).unwrap_or_default();
        let encodings: BTreeMap<Vec<u8>, Encoding<'_>> = fonts
            .iter()
            .filter_map(|(name, font)| {
                font.get_font_encoding_with_limit(pdf, stream_limit)
                    .ok()
                    .map(|enc| (name.clone(), enc))
            })
            .collect();

        let page_resources = Self::resolve_page_resources(pdf, page_id);

        let mut out = String::new();
        let mut state = PdfTextState::default();
        let mut state_stack: Vec<PdfTextState> = Vec::new();
        let mut tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut tlm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        // Previous run end in user space: (end_x, baseline_y, effective_em_size)
        let mut last_pos: Option<(f64, f64, f64)> = None;

        let mut ctx = PdfExtractContext {
            pdf,
            stream_limit,
            state: &mut state,
            state_stack: &mut state_stack,
            tm: &mut tm,
            tlm: &mut tlm,
            last_pos: &mut last_pos,
            out: &mut out,
            forms_invoked: 0,
            active_forms: Vec::new(),
        };

        ctx.walk_operations(&content.operations, page_resources, None, &encodings, 0);

        Some(out)
    }

    /// Walk up the page tree from `page_id` to find the effective `/Resources` dictionary.
    fn resolve_page_resources(pdf: &LopdfDocument, page_id: ObjectId) -> Option<&Dictionary> {
        let mut current_id = page_id;
        for _ in 0..32 {
            let dict = pdf.get_dictionary(current_id).ok()?;
            if let Ok(res_obj) = dict.get(b"Resources")
                && let Some(res_dict) = Self::deref_dict(pdf, res_obj)
            {
                return Some(res_dict);
            }
            match dict.get(b"Parent").and_then(Object::as_reference) {
                Ok(parent_id) if parent_id != current_id => current_id = parent_id,
                _ => break,
            }
        }
        None
    }

    /// Resolve a direct or indirect PDF `Object` as a `&Dictionary`.
    fn deref_dict<'a>(pdf: &'a LopdfDocument, obj: &'a Object) -> Option<&'a Dictionary> {
        match obj {
            Object::Dictionary(d) => Some(d),
            Object::Reference(id) => pdf.get_dictionary(*id).ok(),
            _ => None,
        }
    }
}

/// Mutable state for recursive PDF content stream and Form XObject text extraction
struct PdfExtractContext<'a> {
    pdf: &'a LopdfDocument,
    stream_limit: usize,
    state: &'a mut PdfTextState,
    state_stack: &'a mut Vec<PdfTextState>,
    tm: &'a mut [f64; 6],
    tlm: &'a mut [f64; 6],
    last_pos: &'a mut Option<(f64, f64, f64)>,
    out: &'a mut String,
    /// Form XObjects invoked so far on this page (see [`MAX_FORM_XOBJECT_INVOCATIONS`]).
    forms_invoked: usize,
    /// Indirect forms currently being walked, to skip a form that invokes itself.
    active_forms: Vec<ObjectId>,
}

const MAX_FORM_XOBJECT_DEPTH: usize = 16;
/// Total Form XObject invocations per page. The depth limit alone does not bound the work: a
/// form that invokes another form several times per level grows exponentially with depth.
const MAX_FORM_XOBJECT_INVOCATIONS: usize = 4096;

impl<'a> PdfExtractContext<'a> {
    fn emit_run(
        &mut self,
        bytes: &[u8],
        local_encodings: Option<&BTreeMap<Vec<u8>, Encoding<'_>>>,
        page_encodings: &BTreeMap<Vec<u8>, Encoding<'_>>,
    ) {
        let decoded = decode_pdf_bytes(
            bytes,
            self.state.font_name.as_deref(),
            local_encodings,
            page_encodings,
        );
        if decoded.is_empty() {
            return;
        }

        let m = multiply_matrix(*self.tm, self.state.ctm);
        let x = m[4];
        let y = m[5];
        let scale = m[0].hypot(m[1]).max(m[2].hypot(m[3])).max(1e-6);
        let em = (self.state.font_size.abs().max(1.0) * scale).max(1.0);

        if let Some((prev_end_x, prev_y, prev_em)) = *self.last_pos {
            let line_em = prev_em.max(em);
            if (y - prev_y).abs() > line_em * 0.45 {
                while self.out.ends_with(' ') {
                    self.out.pop();
                }
                if !self.out.is_empty() && !self.out.ends_with('\n') {
                    self.out.push('\n');
                }
            } else {
                let gap = x - prev_end_x;
                if gap > line_em * 0.22
                    && !self.out.is_empty()
                    && !self.out.ends_with([' ', '\n'])
                    && !decoded.starts_with(' ')
                {
                    self.out.push(' ');
                }
            }
        }

        self.out.push_str(&decoded);
        let adv = estimate_text_advance(&decoded, self.state);
        *self.tm = multiply_matrix([1.0, 0.0, 0.0, 1.0, adv, 0.0], *self.tm);
        let end_m = multiply_matrix(*self.tm, self.state.ctm);
        *self.last_pos = Some((end_m[4], end_m[5], em));
    }

    #[allow(clippy::too_many_lines)]
    fn walk_operations(
        &mut self,
        operations: &[lopdf::content::Operation],
        resources: Option<&'a Dictionary>,
        local_encodings: Option<&BTreeMap<Vec<u8>, Encoding<'a>>>,
        page_encodings: &BTreeMap<Vec<u8>, Encoding<'a>>,
        depth: usize,
    ) {
        for op in operations {
            match op.operator.as_str() {
                "q" => self.state_stack.push(self.state.clone()),
                "Q" => {
                    if let Some(prev) = self.state_stack.pop() {
                        *self.state = prev;
                    }
                }
                "cm" if op.operands.len() >= 6 => {
                    let mut m = [0.0; 6];
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        m[i] = obj_to_f64(operand).unwrap_or(0.0);
                    }
                    self.state.ctm = multiply_matrix(m, self.state.ctm);
                }
                "BT" => {
                    *self.tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                    *self.tlm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                }
                "Tf" if op.operands.len() >= 2 => {
                    self.state.font_name = op
                        .operands
                        .first()
                        .and_then(|o| o.as_name().ok())
                        .map(<[u8]>::to_vec);
                    if let Some(sz) = op.operands.get(1).and_then(obj_to_f64) {
                        self.state.font_size = sz;
                    }
                }
                "Tc" => {
                    if let Some(v) = op.operands.first().and_then(obj_to_f64) {
                        self.state.char_space = v;
                    }
                }
                "Tw" => {
                    if let Some(v) = op.operands.first().and_then(obj_to_f64) {
                        self.state.word_space = v;
                    }
                }
                "Tz" => {
                    if let Some(v) = op.operands.first().and_then(obj_to_f64) {
                        self.state.horiz_scale = v / 100.0;
                    }
                }
                "TL" => {
                    if let Some(v) = op.operands.first().and_then(obj_to_f64) {
                        self.state.leading = v;
                    }
                }
                "Td" if op.operands.len() >= 2 => {
                    let tx = obj_to_f64(&op.operands[0]).unwrap_or(0.0);
                    let ty = obj_to_f64(&op.operands[1]).unwrap_or(0.0);
                    *self.tlm = multiply_matrix([1.0, 0.0, 0.0, 1.0, tx, ty], *self.tlm);
                    *self.tm = *self.tlm;
                }
                "TD" if op.operands.len() >= 2 => {
                    let tx = obj_to_f64(&op.operands[0]).unwrap_or(0.0);
                    let ty = obj_to_f64(&op.operands[1]).unwrap_or(0.0);
                    self.state.leading = -ty;
                    *self.tlm = multiply_matrix([1.0, 0.0, 0.0, 1.0, tx, ty], *self.tlm);
                    *self.tm = *self.tlm;
                }
                "Tm" if op.operands.len() >= 6 => {
                    for (i, operand) in op.operands.iter().take(6).enumerate() {
                        self.tm[i] = obj_to_f64(operand).unwrap_or(0.0);
                    }
                    *self.tlm = *self.tm;
                }
                "T*" => {
                    *self.tlm =
                        multiply_matrix([1.0, 0.0, 0.0, 1.0, 0.0, -self.state.leading], *self.tlm);
                    *self.tm = *self.tlm;
                }
                "Tj" => {
                    if let Some(Object::String(bytes, _)) = op.operands.first() {
                        self.emit_run(bytes, local_encodings, page_encodings);
                    }
                }
                "'" => {
                    *self.tlm =
                        multiply_matrix([1.0, 0.0, 0.0, 1.0, 0.0, -self.state.leading], *self.tlm);
                    *self.tm = *self.tlm;
                    if let Some(Object::String(bytes, _)) = op.operands.first() {
                        self.emit_run(bytes, local_encodings, page_encodings);
                    }
                }
                "\"" if op.operands.len() >= 3 => {
                    if let Some(w) = obj_to_f64(&op.operands[0]) {
                        self.state.word_space = w;
                    }
                    if let Some(c) = obj_to_f64(&op.operands[1]) {
                        self.state.char_space = c;
                    }
                    *self.tlm =
                        multiply_matrix([1.0, 0.0, 0.0, 1.0, 0.0, -self.state.leading], *self.tlm);
                    *self.tm = *self.tlm;
                    if let Object::String(bytes, _) = &op.operands[2] {
                        self.emit_run(bytes, local_encodings, page_encodings);
                    }
                }
                "TJ" => {
                    if let Some(Object::Array(items)) = op.operands.first() {
                        for item in items {
                            match item {
                                Object::String(bytes, _) => {
                                    self.emit_run(bytes, local_encodings, page_encodings);
                                }
                                Object::Integer(_) | Object::Real(_) => {
                                    if let Some(adj) = obj_to_f64(item) {
                                        let tx = -(adj / 1000.0)
                                            * self.state.font_size
                                            * self.state.horiz_scale;
                                        *self.tm = multiply_matrix(
                                            [1.0, 0.0, 0.0, 1.0, tx, 0.0],
                                            *self.tm,
                                        );
                                        if adj <= -180.0
                                            && !self.out.is_empty()
                                            && !self.out.ends_with([' ', '\n'])
                                        {
                                            self.out.push(' ');
                                        }
                                        if let Some((_, prev_y, prev_em)) = *self.last_pos {
                                            let end_m = multiply_matrix(*self.tm, self.state.ctm);
                                            *self.last_pos = Some((end_m[4], prev_y, prev_em));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "Do" if depth < MAX_FORM_XOBJECT_DEPTH => {
                    if let Some(xobj_name) = op.operands.first().and_then(|o| o.as_name().ok()) {
                        self.invoke_form_xobject(xobj_name, resources, page_encodings, depth);
                    }
                }
                _ => {}
            }
        }
    }

    fn invoke_form_xobject(
        &mut self,
        xobj_name: &[u8],
        resources: Option<&'a Dictionary>,
        page_encodings: &BTreeMap<Vec<u8>, Encoding<'a>>,
        depth: usize,
    ) {
        let Some(res_dict) = resources else {
            return;
        };
        let Ok(xobjects_obj) = res_dict.get(b"XObject") else {
            return;
        };
        let Some(xobjects_dict) = PdfParser::deref_dict(self.pdf, xobjects_obj) else {
            return;
        };
        let Ok(target_obj) = xobjects_dict.get(xobj_name) else {
            return;
        };
        let (stream, form_id) = match target_obj {
            Object::Stream(s) => (s, None),
            Object::Reference(id) => match self.pdf.get_object(*id) {
                Ok(Object::Stream(s)) => (s, Some(*id)),
                _ => return,
            },
            _ => return,
        };
        if self.forms_invoked >= MAX_FORM_XOBJECT_INVOCATIONS
            || form_id.is_some_and(|id| self.active_forms.contains(&id))
        {
            return;
        }
        self.forms_invoked += 1;

        let is_form = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|subtype| subtype == b"Form");
        if !is_form {
            return;
        }

        let Ok(form_bytes) = stream.decompressed_content_with_limit(self.stream_limit) else {
            return;
        };
        let Ok(form_content) = Content::decode(&form_bytes) else {
            return;
        };

        let saved_state = self.state.clone();
        let saved_stack_len = self.state_stack.len();

        if let Ok(Object::Array(matrix_arr)) = stream.dict.get(b"Matrix")
            && matrix_arr.len() >= 6
        {
            let mut m = [0.0; 6];
            for (i, item) in matrix_arr.iter().take(6).enumerate() {
                m[i] = obj_to_f64(item).unwrap_or(0.0);
            }
            self.state.ctm = multiply_matrix(m, self.state.ctm);
        }

        let form_resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|obj| PdfParser::deref_dict(self.pdf, obj))
            .or(resources);

        // Extract any font encodings defined in the Form XObject's own /Resources/Font dictionary
        let mut form_encodings: Option<BTreeMap<Vec<u8>, Encoding<'a>>> = None;
        if let Some(f_res) = form_resources
            && let Ok(font_obj) = f_res.get(b"Font")
            && let Some(font_dict) = PdfParser::deref_dict(self.pdf, font_obj)
        {
            let mut map = BTreeMap::new();
            for (font_name, font_ref) in font_dict {
                if let Some(f_dict) = PdfParser::deref_dict(self.pdf, font_ref)
                    && let Ok(enc) =
                        f_dict.get_font_encoding_with_limit(self.pdf, self.stream_limit)
                {
                    map.insert(font_name.clone(), enc);
                }
            }
            form_encodings = Some(map);
        }

        if let Some(id) = form_id {
            self.active_forms.push(id);
        }
        self.walk_operations(
            &form_content.operations,
            form_resources,
            form_encodings.as_ref(),
            page_encodings,
            depth + 1,
        );
        if form_id.is_some() {
            self.active_forms.pop();
        }

        self.state_stack.truncate(saved_stack_len);
        *self.state = saved_state;
    }
}

/// Read a text string entry (`PDFDocEncoding` or UTF-16BE) from an info dictionary
fn info_string(info: &Dictionary, key: &[u8]) -> Option<String> {
    let value = lopdf::decode_text_string(info.get(key).ok()?).ok()?;
    let value = value.trim_matches(char::from(0)).trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Parse a PDF date string (`D:YYYYMMDDHHmmSSOHH'mm'`); every part after the
/// year is optional.
fn parse_pdf_date(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim().strip_prefix("D:").unwrap_or(raw.trim());
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    let field = |range: std::ops::Range<usize>, default: u32| {
        digits
            .get(range)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default)
    };
    let year = i32::try_from(field(0..4, 0)).ok().filter(|&y| y > 0)?;
    let naive = NaiveDate::from_ymd_opt(year, field(4..6, 1), field(6..8, 1))?.and_hms_opt(
        field(8..10, 0),
        field(10..12, 0),
        field(12..14, 0),
    )?;

    let zone = &s[digits.len()..];
    let offset_seconds = match zone.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let parts: Vec<i32> = zone[1..]
                .split('\'')
                .filter_map(|p| p.parse().ok())
                .collect();
            let seconds = parts.first().copied().unwrap_or(0) * 3600
                + parts.get(1).copied().unwrap_or(0) * 60;
            if sign == '-' { -seconds } else { seconds }
        }
        _ => 0,
    };
    let offset = FixedOffset::east_opt(offset_seconds)?;
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

impl Default for PdfParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Parser for PdfParser {
    fn format(&self) -> Format {
        Format::pdf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        data.starts_with(b"%PDF-")
    }

    /// # Errors
    ///
    /// Returns an error if the data does not start with a PDF signature.
    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        debug!("Parsing PDF, size: {} bytes", context.size);

        if !self.can_parse(&data) {
            return Err(Error::ParseError("Invalid PDF signature".to_string()));
        }

        let stream_limit = crate::limits::max_decompressed_bytes(&context);
        let mut metadata = Metadata::default();
        metadata.add_custom("format", "PDF");

        // A PDF that lopdf cannot read may still render in the viewer, so a
        // load failure only costs text and metadata.
        let text = match LopdfDocument::load_mem(&data) {
            Ok(mut pdf) => {
                let was_encrypted = pdf.is_encrypted();
                if was_encrypted {
                    let _ = pdf.decrypt("");
                }
                Self::extract_metadata(&pdf, &mut metadata);
                metadata.add_custom(
                    "page_count",
                    i64::try_from(pdf.get_pages().len()).unwrap_or(0),
                );
                metadata.add_custom("encrypted", was_encrypted);
                Some(Self::extract_text(&pdf, stream_limit)).filter(|t| !t.trim().is_empty())
            }
            Err(e) => {
                warn!("lopdf could not load PDF ({e}); text and metadata unavailable");
                metadata.add_custom("load_error", e.to_string());
                None
            }
        };

        if metadata.title.is_none() {
            metadata.title.clone_from(&context.filename);
        }

        let mut page = Page::new(1, Dimensions::LETTER);
        page.add_content(ContentBlock::Embedded(EmbeddedBlock::pdf(
            data.to_vec(),
            text,
        )));

        let mut document = Document::new();
        document.pages = vec![page];
        document.metadata = metadata;

        info!("Prepared PDF for native rendering");
        Ok(document)
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "PDF Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pdf_dates() {
        let dt = parse_pdf_date("D:20230115103000+02'00'").unwrap();
        assert_eq!(dt.to_rfc3339(), "2023-01-15T08:30:00+00:00");
        let dt = parse_pdf_date("D:2023").unwrap();
        assert_eq!(dt.to_rfc3339(), "2023-01-01T00:00:00+00:00");
        assert!(parse_pdf_date("garbage").is_none());
    }

    #[tokio::test]
    async fn extracts_text_not_payload() {
        let pdf = minimal_pdf("Hello Prism");
        let parser = PdfParser::new();
        let context = ParseContext {
            format: Format::pdf(),
            filename: Some("t.pdf".to_string()),
            size: pdf.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let document = parser.parse(Bytes::from(pdf), context).await.unwrap();
        let text = document.extract_text();
        assert!(text.contains("Hello Prism"), "got {text:?}");
        assert!(!text.contains("JVBER"), "base64 payload leaked into text");
        assert!(matches!(
            document.pages[0].content[0],
            ContentBlock::Embedded(_)
        ));
    }

    #[tokio::test]
    async fn extracts_text_from_form_xobject() {
        use lopdf::content::{Content, Operation};
        use lopdf::{Object, Stream, dictionary};

        let mut doc = LopdfDocument::with_version("1.5");
        let page_tree_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let form_content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 14.into()]),
                Operation::new("Td", vec![10.into(), 50.into()]),
                Operation::new(
                    "Tj",
                    vec![Object::string_literal("FormXObject Secret Text")],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        let form_stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
                "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 72.into(), 600.into()],
                "Resources" => dictionary! {
                    "Font" => dictionary! { "F1" => font_id },
                },
            },
            form_content.encode().unwrap(),
        );
        let form_id = doc.add_object(form_stream);
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Fm1" => form_id },
        });
        let page_content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 18.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal("Main Page Header")]),
                Operation::new("ET", vec![]),
                Operation::new("Do", vec!["Fm1".into()]),
            ],
        };
        let content_id =
            doc.add_object(Stream::new(dictionary! {}, page_content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => page_tree_id, "Contents" => content_id,
        });
        doc.objects.insert(
            page_tree_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
                "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id =
            doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => page_tree_id });
        doc.trailer.set("Root", catalog_id);
        let mut pdf_bytes = Vec::new();
        doc.save_to(&mut pdf_bytes).unwrap();

        let parser = PdfParser::new();
        let context = ParseContext {
            format: Format::pdf(),
            filename: Some("xobj.pdf".to_string()),
            size: pdf_bytes.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let document = parser.parse(Bytes::from(pdf_bytes), context).await.unwrap();
        let text = document.extract_text();
        assert!(text.contains("Main Page Header"), "got {text:?}");
        assert!(
            text.contains("FormXObject Secret Text"),
            "Form XObject text was not extracted: {text:?}"
        );
    }

    #[tokio::test]
    async fn nested_form_xobjects_cannot_multiply_the_work() {
        use lopdf::content::{Content, Operation};
        use lopdf::{Object, Stream, dictionary};

        // Forms F0..F15, each drawing a word and invoking the next form ten times: 10^15
        // invocations without a budget. F15 also invokes F0, which the cycle check skips.
        let mut doc = LopdfDocument::with_version("1.5");
        let page_tree_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let ids: Vec<ObjectId> = (0..16).map(|_| doc.new_object_id()).collect();
        for (i, &id) in ids.iter().enumerate() {
            let next = ids[(i + 1) % ids.len()];
            let mut ops = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 10.into()]),
                Operation::new("Tj", vec![Object::string_literal("w")]),
                Operation::new("ET", vec![]),
            ];
            ops.extend((0..10).map(|_| Operation::new("Do", vec!["Next".into()])));
            let stream = Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                    "Resources" => dictionary! {
                        "Font" => dictionary! { "F1" => font_id },
                        "XObject" => dictionary! { "Next" => next },
                    },
                },
                Content { operations: ops }.encode().unwrap(),
            );
            doc.objects.insert(id, Object::Stream(stream));
        }
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
            "XObject" => dictionary! { "Next" => ids[0] },
        });
        let page_content = Content {
            operations: vec![Operation::new("Do", vec!["Next".into()])],
        };
        let content_id =
            doc.add_object(Stream::new(dictionary! {}, page_content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => page_tree_id, "Contents" => content_id,
        });
        doc.objects.insert(
            page_tree_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
                "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id =
            doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => page_tree_id });
        doc.trailer.set("Root", catalog_id);
        let mut pdf_bytes = Vec::new();
        doc.save_to(&mut pdf_bytes).unwrap();

        let context = ParseContext {
            format: Format::pdf(),
            filename: Some("forms.pdf".to_string()),
            size: pdf_bytes.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let start = std::time::Instant::now();
        let document = PdfParser::new()
            .parse(Bytes::from(pdf_bytes), context)
            .await
            .unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        let words = document.extract_text().matches('w').count();
        assert!(
            words > 0 && words <= MAX_FORM_XOBJECT_INVOCATIONS,
            "{words}"
        );
    }

    #[tokio::test]
    async fn joins_split_runs_on_same_line() {
        let data = std::fs::read("../../test-files/testComment.pdf").unwrap();
        let parser = PdfParser::new();
        let context = ParseContext {
            format: Format::pdf(),
            filename: Some("testComment.pdf".to_string()),
            size: data.len(),
            options: prism_core::parser::ParseOptions::default(),
        };
        let document = parser.parse(Bytes::from(data), context).await.unwrap();
        let text = document.extract_text();
        assert!(
            text.contains("Here is some"),
            "expected 'Here is some' without mid-word split, got {text:?}"
        );
        assert!(
            !text.contains("H \nere"),
            "unexpected split 'H \\nere' in {text:?}"
        );
    }

    /// Build a one-page PDF with a single line of text
    fn minimal_pdf(text: &str) -> Vec<u8> {
        use lopdf::content::{Content, Operation};
        use lopdf::{Object, Stream, dictionary};

        let mut doc = LopdfDocument::with_version("1.5");
        let page_tree_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => page_tree_id, "Contents" => content_id,
        });
        doc.objects.insert(
            page_tree_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
                "Resources" => resources_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id =
            doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => page_tree_id });
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }
}
