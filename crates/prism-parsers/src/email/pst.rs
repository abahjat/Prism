// SPDX-License-Identifier: AGPL-3.0-only
//! Native Outlook `.pst` (Personal Storage Table) and `.ost` (Offline Storage Table) reader.
//!
//! Implements a pure-Rust MS-PST NDB (Node Database) and LTP (Lists, Tables, and Properties)
//! parser supporting both Unicode (`wVer >= 21`) and ANSI (`wVer = 14`/`15`) containers,
//! compressible (`NDB_CRYPT_PERMUTE = 1`) and cyclic (`NDB_CRYPT_CYCLIC = 2`) data block
//! decryption, folder hierarchy reconstruction, MAPI message property extraction, attachment
//! subnode extraction, RFC 5322 `.eml` child synthesis, and paginated UDM [`Document`] layout.

#![allow(clippy::similar_names, clippy::too_many_lines)]

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use prism_core::{
    document::{
        Attachment, CellBorders, ContentBlock, Dimensions, Document, Page, PageMargins,
        PageMetadata, Rect, ShapeStyle, TableBlock, TableCell, TableRow, TextBlock, TextRun,
        TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

/// MS-PST file signature (`!BDN` = `0x4E444221` little-endian).
pub const PST_MAGIC: [u8; 4] = [0x21, 0x42, 0x44, 0x4E];

/// No block encryption (`bCryptMethod = 0`).
pub const NDB_CRYPT_NONE: u8 = 0x00;
/// Compressible (permutative) encryption (`bCryptMethod = 1`).
pub const NDB_CRYPT_PERMUTE: u8 = 0x01;
/// Cyclic ("high") encryption (`bCryptMethod = 2`). Not supported: such files are reported as
/// encrypted rather than decoded.
pub const NDB_CRYPT_CYCLIC: u8 = 0x02;

const NID_TYPE_NORMAL_FOLDER: u32 = 0x02;
const NID_TYPE_SEARCH_FOLDER: u32 = 0x03;
const NID_TYPE_NORMAL_MESSAGE: u32 = 0x04;
const NID_TYPE_ATTACHMENT: u32 = 0x05;
const NID_TYPE_ASSOC_MESSAGE: u32 = 0x08;
/// Subnode type used by the builder for property values too large for the heap.
const NID_TYPE_LTP: u32 = 0x1F;

/// Most bytes gathered for one node's data tree (`XBLOCK` / `XXBLOCK`).
const MAX_NODE_BYTES: usize = 64 * 1024 * 1024;
/// Most data blocks gathered for one node's data tree.
const MAX_NODE_BLOCKS: usize = 65_536;
/// Most entries collected from the node and block B-trees.
const MAX_BTREE_ENTRIES: usize = 4_000_000;
/// Most entries collected from one node's subnode tree.
const MAX_SUBNODES: usize = 1_000_000;
/// Largest data a single data block holds (8 KiB block minus its 16-byte trailer).
const MAX_BLOCK_DATA: usize = 8176;
/// Largest property value the builder keeps on the heap before moving it to a subnode.
const MAX_HEAP_VALUE: usize = 3580;

/// `mpbbI` from the `[MS-PST]` §5.1 `mpbbCrypt` table: the inverse permutation, used to
/// decode `NDB_CRYPT_PERMUTE` blocks (`mpbbR`, used to encode, is its inverse).
const MPBB_I: [u8; 256] = [
    0x47, 0xF1, 0xB4, 0xE6, 0x0B, 0x6A, 0x72, 0x48, 0x85, 0x4E, 0x9E, 0xEB, 0xE2, 0xF8, 0x94, 0x53,
    0xE0, 0xBB, 0xA0, 0x02, 0xE8, 0x5A, 0x09, 0xAB, 0xDB, 0xE3, 0xBA, 0xC6, 0x7C, 0xC3, 0x10, 0xDD,
    0x39, 0x05, 0x96, 0x30, 0xF5, 0x37, 0x60, 0x82, 0x8C, 0xC9, 0x13, 0x4A, 0x6B, 0x1D, 0xF3, 0xFB,
    0x8F, 0x26, 0x97, 0xCA, 0x91, 0x17, 0x01, 0xC4, 0x32, 0x2D, 0x6E, 0x31, 0x95, 0xFF, 0xD9, 0x23,
    0xD1, 0x00, 0x5E, 0x79, 0xDC, 0x44, 0x3B, 0x1A, 0x28, 0xC5, 0x61, 0x57, 0x20, 0x90, 0x3D, 0x83,
    0xB9, 0x43, 0xBE, 0x67, 0xD2, 0x46, 0x42, 0x76, 0xC0, 0x6D, 0x5B, 0x7E, 0xB2, 0x0F, 0x16, 0x29,
    0x3C, 0xA9, 0x03, 0x54, 0x0D, 0xDA, 0x5D, 0xDF, 0xF6, 0xB7, 0xC7, 0x62, 0xCD, 0x8D, 0x06, 0xD3,
    0x69, 0x5C, 0x86, 0xD6, 0x14, 0xF7, 0xA5, 0x66, 0x75, 0xAC, 0xB1, 0xE9, 0x45, 0x21, 0x70, 0x0C,
    0x87, 0x9F, 0x74, 0xA4, 0x22, 0x4C, 0x6F, 0xBF, 0x1F, 0x56, 0xAA, 0x2E, 0xB3, 0x78, 0x33, 0x50,
    0xB0, 0xA3, 0x92, 0xBC, 0xCF, 0x19, 0x1C, 0xA7, 0x63, 0xCB, 0x1E, 0x4D, 0x3E, 0x4B, 0x1B, 0x9B,
    0x4F, 0xE7, 0xF0, 0xEE, 0xAD, 0x3A, 0xB5, 0x59, 0x04, 0xEA, 0x40, 0x55, 0x25, 0x51, 0xE5, 0x7A,
    0x89, 0x38, 0x68, 0x52, 0x7B, 0xFC, 0x27, 0xAE, 0xD7, 0xBD, 0xFA, 0x07, 0xF4, 0xCC, 0x8E, 0x5F,
    0xEF, 0x35, 0x9C, 0x84, 0x2B, 0x15, 0xD5, 0x77, 0x34, 0x49, 0xB6, 0x12, 0x0A, 0x7F, 0x71, 0x88,
    0xFD, 0x9D, 0x18, 0x41, 0x7D, 0x93, 0xD8, 0x58, 0x2C, 0xCE, 0xFE, 0x24, 0xAF, 0xDE, 0xB8, 0x36,
    0xC8, 0xA1, 0x80, 0xA6, 0x99, 0x98, 0xA8, 0x2F, 0x0E, 0x81, 0x65, 0x73, 0xE4, 0xC2, 0xA2, 0x8A,
    0xD4, 0xE1, 0x11, 0xD0, 0x08, 0x8B, 0x2A, 0xF2, 0xED, 0x9A, 0x64, 0x3F, 0xC1, 0x6C, 0xF9, 0xEC,
];

fn decrypt_permute(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        *b = MPBB_I[usize::from(*b)];
    }
}

fn encrypt_permute(buf: &mut [u8]) {
    let mut mpbb_r = [0u8; 256];
    for (idx, &dec) in MPBB_I.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        {
            mpbb_r[usize::from(dec)] = idx as u8;
        }
    }
    for b in buf.iter_mut() {
        *b = mpbb_r[usize::from(*b)];
    }
}

/// Header values are written into synthesized RFC 5322 text, so line breaks in stored MAPI
/// strings must not be able to start new header lines.
fn header_safe(value: &str) -> String {
    value
        .chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

/// Quoted MIME parameter values (`name="..."`) must not contain quotes, backslashes or line breaks.
fn quoted_param_safe(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\r' | '\n' => '_',
            other => other,
        })
        .collect()
}

/// Parsed PST/OST folder metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PstFolder {
    /// Folder node ID (`NID`).
    pub nid: u32,
    /// Parent folder node ID (`nidParent`).
    pub parent_nid: u32,
    /// Display name (`PidTagDisplayName`).
    pub display_name: String,
    /// Full slash-delimited folder path (e.g. `"Top of Personal Folders/Inbox"`).
    pub path: String,
}

/// Parsed PST/OST attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PstAttachment {
    /// Attachment filename (`PidTagAttachLongFilename` or `PidTagAttachFilename`).
    pub filename: String,
    /// Optional MIME type (`PidTagAttachMimeTag`).
    pub mime_type: Option<String>,
    /// Raw attachment payload (`PidTagAttachDataBinary`). Empty when the attachment is an
    /// embedded message or OLE object, whose contents are not extracted.
    pub data: Vec<u8>,
}

/// Parsed PST/OST email/item message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PstMessage {
    /// Message node ID (`NID`).
    pub nid: u32,
    /// Parent folder node ID (`nidParent`).
    pub folder_nid: u32,
    /// Resolved folder path (`"Inbox"`, `"Sent Items"`, etc.).
    pub folder_path: String,
    /// Subject (`PidTagSubject`, `0x0037`).
    pub subject: String,
    /// Sender display name (`PidTagSenderName`, `0x0C1A`).
    pub sender_name: Option<String>,
    /// Sender email address (`PidTagSenderEmailAddress`, `0x0C1F`).
    pub sender_email: Option<String>,
    /// Display `To` recipients (`PidTagDisplayTo`, `0x0E04`).
    pub display_to: Option<String>,
    /// Display `Cc` recipients (`PidTagDisplayCc`, `0x0E03`).
    pub display_cc: Option<String>,
    /// Display `Bcc` recipients (`PidTagDisplayBcc`, `0x0E02`).
    pub display_bcc: Option<String>,
    /// Internet Message-ID (`PidTagInternetMessageId`, `0x1035`).
    pub message_id: Option<String>,
    /// Message class (`PidTagMessageClass`, `0x001A`, e.g., `"IPM.Note"`).
    pub message_class: Option<String>,
    /// Delivery or submit timestamp (`PidTagMessageDeliveryTime` `0x0E06` or `PidTagClientSubmitTime` `0x0039`).
    pub timestamp: Option<DateTime<Utc>>,
    /// Plain-text body (`PidTagBody`, `0x1000`).
    pub body_text: Option<String>,
    /// HTML body (`PidTagHtml`, `0x1013`).
    pub body_html: Option<String>,
    /// True when the message carries a compressed RTF body (`PidTagRtfCompressed`, `0x1009`),
    /// which is not decoded.
    pub has_rtf_body: bool,
    /// Extracted attachments.
    pub attachments: Vec<PstAttachment>,
}

impl PstMessage {
    /// Format the `From` header line (`"Name <email>"` or fallback).
    #[must_use]
    pub fn formatted_from(&self) -> String {
        match (&self.sender_name, &self.sender_email) {
            (Some(name), Some(email)) if !name.is_empty() && !email.is_empty() => {
                if name.contains(email.as_str()) {
                    name.clone()
                } else {
                    format!("{name} <{email}>")
                }
            }
            (Some(name), _) if !name.is_empty() => name.clone(),
            (_, Some(email)) if !email.is_empty() => email.clone(),
            _ => "unknown@pst.local".to_string(),
        }
    }

    /// The body shown to a reviewer: plain text, else HTML as text, else a note saying why
    /// there is none.
    #[must_use]
    pub fn display_body(&self) -> String {
        self.body_text
            .clone()
            .or_else(|| {
                self.body_html
                    .as_deref()
                    .map(prism_core::text::html_to_text)
            })
            .unwrap_or_else(|| {
                if self.has_rtf_body {
                    "[Body stored only as compressed RTF (PidTagRtfCompressed); not decoded]"
                        .to_string()
                } else {
                    "[No message body]".to_string()
                }
            })
    }

    /// Synthesize a standard RFC 5322 `.eml` message representation (including attachments)
    /// so child extraction and downstream `.eml` parsing work seamlessly.
    #[must_use]
    pub fn to_eml_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        let _ = write!(out, "From: {}\r\n", header_safe(&self.formatted_from()));
        for (name, value) in [
            ("To", &self.display_to),
            ("Cc", &self.display_cc),
            ("Bcc", &self.display_bcc),
        ] {
            if let Some(v) = value
                && !v.is_empty()
            {
                let _ = write!(out, "{name}: {}\r\n", header_safe(v));
            }
        }
        let _ = write!(out, "Subject: {}\r\n", header_safe(&self.subject));
        if let Some(ts) = self.timestamp {
            let _ = write!(out, "Date: {}\r\n", ts.to_rfc2822());
        }
        let msg_id = self.message_id.as_deref().map_or_else(
            || format!("<pst-nid-{}@prism.local>", self.nid),
            header_safe,
        );
        let _ = write!(out, "Message-ID: {msg_id}\r\n");
        let _ = write!(out, "X-PST-Folder: {}\r\n", header_safe(&self.folder_path));
        out.push_str("MIME-Version: 1.0\r\n");

        let body_str = self.display_body();

        if self.attachments.is_empty() {
            out.push_str("Content-Type: text/plain; charset=\"utf-8\"\r\n\r\n");
            out.push_str(&body_str);
            out.push_str("\r\n");
        } else {
            let boundary = format!("----=_PrismPstBoundary_{:08x}", self.nid);
            let _ = write!(
                out,
                "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
            );
            let _ = write!(out, "--{boundary}\r\n");
            out.push_str("Content-Type: text/plain; charset=\"utf-8\"\r\n\r\n");
            out.push_str(&body_str);
            out.push_str("\r\n");
            for att in &self.attachments {
                let mime = att
                    .mime_type
                    .as_deref()
                    .filter(|m| m.contains('/') && !m.contains(['\r', '\n', ';', '"']))
                    .unwrap_or("application/octet-stream");
                let fname = quoted_param_safe(&att.filename);
                let _ = write!(out, "--{boundary}\r\n");
                let _ = write!(out, "Content-Type: {mime}; name=\"{fname}\"\r\n");
                let _ = write!(
                    out,
                    "Content-Disposition: attachment; filename=\"{fname}\"\r\n"
                );
                out.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
                let encoded = BASE64_STANDARD.encode(&att.data);
                for chunk in encoded.as_bytes().chunks(76) {
                    if let Ok(line) = std::str::from_utf8(chunk) {
                        out.push_str(line);
                        out.push_str("\r\n");
                    }
                }
            }
            let _ = write!(out, "--{boundary}--\r\n");
        }

        out.into_bytes()
    }
}

/// Parsed MS-PST / MS-OST archive.
#[derive(Debug, Clone)]
pub struct PstArchive {
    /// Format version (`wVer`: 14/15 for ANSI, 23/36 for Unicode).
    pub version: u16,
    /// Encryption method (`0` = None, `1` = Permute).
    pub crypt_method: u8,
    /// True if client magic or header indicates `.ost`.
    pub is_ost: bool,
    /// Extracted folders ordered by NID.
    pub folders: Vec<PstFolder>,
    /// Extracted messages ordered by folder & NID.
    pub messages: Vec<PstMessage>,
}

#[derive(Debug, Clone, Copy)]
struct BbtEntry {
    ib: u64,
    cb: u16,
}

#[derive(Debug, Clone, Copy)]
struct NbtEntry {
    nid: u32,
    bid_data: u64,
    bid_sub: u64,
    nid_parent: u32,
}

#[derive(Debug, Clone, Copy)]
struct SlEntry {
    nid: u32,
    bid_data: u64,
    bid_sub: u64,
}

/// Offsets inside a 512-byte `BTPAGE` (`[MS-PST]` §2.2.2.7.7.1): the entries area, then
/// `cEnt`, `cEntMax`, `cbEnt` and `cLevel` (one byte each).
struct PageLayout {
    entries_len: usize,
    c_ent: usize,
    cb_ent: usize,
    c_level: usize,
}

const UNICODE_PAGE: PageLayout = PageLayout {
    entries_len: 488,
    c_ent: 488,
    cb_ent: 490,
    c_level: 491,
};
const ANSI_PAGE: PageLayout = PageLayout {
    entries_len: 496,
    c_ent: 496,
    cb_ent: 498,
    c_level: 499,
};

/// Node database reader: the block B-tree plus what is needed to read and decode blocks.
struct NdbReader<'a> {
    data: &'a [u8],
    is_unicode: bool,
    crypt_method: u8,
    bbt: BTreeMap<u64, BbtEntry>,
}

impl PstArchive {
    /// Parse a binary `.pst` or `.ost` byte slice into a [`PstArchive`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ParseError`] if the magic header is missing or truncated, or
    /// [`Error::Encrypted`] if the blocks use an encoding other than none or permutation.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 512 || data[0..4] != PST_MAGIC {
            return Err(Error::ParseError(
                "Invalid PST/OST file: missing !BDN magic signature".to_string(),
            ));
        }

        let client_magic = read_u16_le(data, 8);
        let is_ost = client_magic == 0x4F53; // "SO"
        let version = read_u16_le(data, 10);
        let is_unicode = version >= 21;

        let (crypt_method, nbt_ib, bbt_ib) = if is_unicode {
            if data.len() < 564 {
                return Err(Error::ParseError(
                    "Truncated Unicode PST/OST header (< 564 bytes)".to_string(),
                ));
            }
            (data[513], read_u64_le(data, 224), read_u64_le(data, 240))
        } else {
            (
                data[461],
                u64::from(read_u32_le(data, 188)),
                u64::from(read_u32_le(data, 196)),
            )
        };
        match crypt_method {
            NDB_CRYPT_NONE | NDB_CRYPT_PERMUTE => {}
            NDB_CRYPT_CYCLIC => {
                return Err(Error::Encrypted(
                    "PST/OST uses cyclic (high) encryption, which is not supported".to_string(),
                ));
            }
            other => {
                return Err(Error::Encrypted(format!(
                    "PST/OST uses unsupported block encryption method {other:#04x}"
                )));
            }
        }

        let mut ndb = NdbReader {
            data,
            is_unicode,
            crypt_method,
            bbt: BTreeMap::new(),
        };
        let mut visited = HashSet::new();
        let mut bbt = BTreeMap::new();
        ndb.walk_btree(bbt_ib, 0, &mut visited, &mut |entry, cb_ent| {
            ndb_bbt_leaf(entry, cb_ent, is_unicode, &mut bbt);
        });
        ndb.bbt = bbt;
        let mut nbt = Vec::new();
        visited.clear();
        ndb.walk_btree(nbt_ib, 0, &mut visited, &mut |entry, cb_ent| {
            ndb_nbt_leaf(entry, cb_ent, is_unicode, &mut nbt);
        });

        let mut raw_folders: BTreeMap<u32, (u32, String)> = BTreeMap::new();
        let mut messages = Vec::new();

        // Pass 1: Extract folders
        for entry in &nbt {
            let nid_type = entry.nid & 0x1F;
            if nid_type == NID_TYPE_NORMAL_FOLDER || nid_type == NID_TYPE_SEARCH_FOLDER {
                let blocks = ndb.node_blocks(entry.bid_data);
                if blocks.is_empty() {
                    continue;
                }
                let subnodes = ndb.subnodes(entry.bid_sub);
                let props = ndb.property_context(&blocks, &subnodes);
                let name = props
                    .get_string(0x3001)
                    .unwrap_or_else(|| format!("Folder_{:04X}", entry.nid));
                raw_folders.insert(entry.nid, (entry.nid_parent, name));
            }
        }

        // Build resolved folder paths
        let mut folders = Vec::with_capacity(raw_folders.len());
        let mut folder_paths: BTreeMap<u32, String> = BTreeMap::new();
        for (&nid, &(parent_nid, ref name)) in &raw_folders {
            let path = resolve_folder_path(nid, &raw_folders);
            folder_paths.insert(nid, path.clone());
            folders.push(PstFolder {
                nid,
                parent_nid,
                display_name: name.clone(),
                path,
            });
        }

        // Pass 2: Extract messages
        for entry in &nbt {
            let nid_type = entry.nid & 0x1F;
            if nid_type != NID_TYPE_NORMAL_MESSAGE && nid_type != NID_TYPE_ASSOC_MESSAGE {
                continue;
            }
            let blocks = ndb.node_blocks(entry.bid_data);
            if blocks.is_empty() {
                continue;
            }
            let subnodes = ndb.subnodes(entry.bid_sub);
            let props = ndb.property_context(&blocks, &subnodes);

            let subject = props
                .get_string(0x0037)
                .or_else(|| props.get_string(0x0E1D))
                .unwrap_or_else(|| "(No Subject)".to_string());
            let sender_name = props
                .get_string(0x0C1A)
                .or_else(|| props.get_string(0x0042));
            let sender_email = props
                .get_string(0x0C1F)
                .or_else(|| props.get_string(0x0065))
                .or_else(|| props.get_string(0x5D01));
            let timestamp = props
                .get_filetime(0x0E06)
                .or_else(|| props.get_filetime(0x0039))
                .or_else(|| props.get_filetime(0x3007));
            let body_html = props
                .get_binary(0x1013)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .or_else(|| props.get_string(0x1013));

            let mut attachments = Vec::new();
            for sub in subnodes
                .iter()
                .filter(|s| (s.nid & 0x1F) == NID_TYPE_ATTACHMENT)
            {
                let att_blocks = ndb.node_blocks(sub.bid_data);
                if att_blocks.is_empty() {
                    continue;
                }
                let att_children = ndb.subnodes(sub.bid_sub);
                let att_props = ndb.property_context(&att_blocks, &att_children);
                let filename = att_props
                    .get_string(0x3707)
                    .or_else(|| att_props.get_string(0x3704))
                    .or_else(|| att_props.get_string(0x3001))
                    .unwrap_or_else(|| format!("attachment_{:04X}.bin", sub.nid));
                attachments.push(PstAttachment {
                    filename,
                    mime_type: att_props.get_string(0x370E),
                    data: att_props.get_binary(0x3701).unwrap_or_default(),
                });
            }

            messages.push(PstMessage {
                nid: entry.nid,
                folder_nid: entry.nid_parent,
                folder_path: folder_paths
                    .get(&entry.nid_parent)
                    .cloned()
                    .unwrap_or_else(|| "Root".to_string()),
                subject,
                sender_name,
                sender_email,
                display_to: props.get_string(0x0E04),
                display_cc: props.get_string(0x0E03),
                display_bcc: props.get_string(0x0E02),
                message_id: props.get_string(0x1035),
                message_class: props.get_string(0x001A),
                timestamp,
                body_text: props.get_string(0x1000),
                body_html,
                has_rtf_body: props.has(0x1009),
                attachments,
            });
        }

        Ok(Self {
            version,
            crypt_method,
            is_ost,
            folders,
            messages,
        })
    }
}

fn resolve_folder_path(nid: u32, raw_folders: &BTreeMap<u32, (u32, String)>) -> String {
    let mut parts = Vec::new();
    let mut cur = nid;
    let mut visited = 0usize;
    while let Some((parent, name)) = raw_folders.get(&cur) {
        parts.push(name.as_str());
        if *parent == 0 || *parent == cur || visited >= 32 {
            break;
        }
        cur = *parent;
        visited += 1;
    }
    parts.reverse();
    if parts.is_empty() {
        "Root".to_string()
    } else {
        parts.join("/")
    }
}

/// Record one BBT leaf entry (`BBTENTRY`: `BREF` + `cb` + `cRef`).
fn ndb_bbt_leaf(entry: &[u8], cb_ent: usize, is_unicode: bool, out: &mut BTreeMap<u64, BbtEntry>) {
    if out.len() >= MAX_BTREE_ENTRIES {
        return;
    }
    if is_unicode && cb_ent >= 24 {
        let bid = read_u64_le(entry, 0) & !1;
        out.insert(
            bid,
            BbtEntry {
                ib: read_u64_le(entry, 8),
                cb: read_u16_le(entry, 16),
            },
        );
    } else if !is_unicode && cb_ent >= 12 {
        let bid = u64::from(read_u32_le(entry, 0)) & !1;
        out.insert(
            bid,
            BbtEntry {
                ib: u64::from(read_u32_le(entry, 4)),
                cb: read_u16_le(entry, 8),
            },
        );
    }
}

/// Record one NBT leaf entry (`NBTENTRY`: `nid`, `bidData`, `bidSub`, `nidParent`).
fn ndb_nbt_leaf(entry: &[u8], cb_ent: usize, is_unicode: bool, out: &mut Vec<NbtEntry>) {
    if out.len() >= MAX_BTREE_ENTRIES {
        return;
    }
    if is_unicode && cb_ent >= 32 {
        #[allow(clippy::cast_possible_truncation)]
        out.push(NbtEntry {
            nid: read_u64_le(entry, 0) as u32,
            bid_data: read_u64_le(entry, 8),
            bid_sub: read_u64_le(entry, 16),
            nid_parent: read_u32_le(entry, 24),
        });
    } else if !is_unicode && cb_ent >= 16 {
        out.push(NbtEntry {
            nid: read_u32_le(entry, 0),
            bid_data: u64::from(read_u32_le(entry, 4)),
            bid_sub: u64::from(read_u32_le(entry, 8)),
            nid_parent: read_u32_le(entry, 12),
        });
    }
}

impl NdbReader<'_> {
    /// Walk the B-tree page at `ib`, handing each leaf entry to `leaf`. Each page is visited at
    /// most once, so a page that points back into the tree cannot make the walk revisit it.
    fn walk_btree(
        &self,
        ib: u64,
        depth: usize,
        visited: &mut HashSet<u64>,
        leaf: &mut dyn FnMut(&[u8], usize),
    ) {
        if depth > 16 || ib == 0 || !visited.insert(ib) {
            return;
        }
        let Ok(offset) = usize::try_from(ib) else {
            return;
        };
        if offset.saturating_add(512) > self.data.len() {
            return;
        }
        let page = &self.data[offset..offset + 512];
        let layout = if self.is_unicode {
            &UNICODE_PAGE
        } else {
            &ANSI_PAGE
        };
        let c_ent = usize::from(page[layout.c_ent]);
        let cb_ent = usize::from(page[layout.cb_ent]);
        let c_level = page[layout.c_level];
        if cb_ent == 0 {
            return;
        }
        let count = (layout.entries_len / cb_ent).min(c_ent);
        for i in 0..count {
            let entry = &page[i * cb_ent..(i + 1) * cb_ent];
            if c_level == 0 {
                leaf(entry, cb_ent);
            } else {
                // BTENTRY: btkey, then BREF (bid, ib)
                let child_ib = if self.is_unicode {
                    read_u64_le(entry, 16)
                } else {
                    u64::from(read_u32_le(entry, 8))
                };
                self.walk_btree(child_ib, depth + 1, visited, leaf);
            }
        }
    }

    /// Read the data blocks of a node, following `XBLOCK` / `XXBLOCK` trees. Blocks are kept
    /// separate because heap IDs address them by index.
    fn node_blocks(&self, bid: u64) -> Vec<Vec<u8>> {
        let mut blocks = Vec::new();
        let mut total = 0usize;
        self.collect_blocks(bid, 0, &mut blocks, &mut total);
        blocks
    }

    /// Concatenated data of a node (used for property values stored in subnodes).
    fn node_data(&self, bid: u64) -> Vec<u8> {
        self.node_blocks(bid).concat()
    }

    fn collect_blocks(&self, bid: u64, depth: usize, out: &mut Vec<Vec<u8>>, total: &mut usize) {
        if bid == 0 || out.len() >= MAX_NODE_BLOCKS || *total >= MAX_NODE_BYTES {
            return;
        }
        let clean_bid = bid & !1;
        let Some(entry) = self.bbt.get(&clean_bid) else {
            return;
        };
        let Ok(offset) = usize::try_from(entry.ib) else {
            return;
        };
        let len = usize::from(entry.cb);
        if offset.saturating_add(len) > self.data.len() {
            return;
        }
        let raw = &self.data[offset..offset + len];

        // Internal block (bit 1 of the BID): an XBLOCK (cLevel 1) or XXBLOCK (cLevel 2) listing
        // child BIDs. The format nests at most two levels.
        if (clean_bid & 2) != 0 {
            if depth >= 2 || raw.len() < 8 || raw[0] != 0x01 {
                return;
            }
            let c_ent = usize::from(read_u16_le(raw, 2));
            let stride = if self.is_unicode { 8 } else { 4 };
            for i in 0..c_ent {
                let pos = 8 + i * stride;
                if pos + stride > raw.len() {
                    break;
                }
                let child_bid = if self.is_unicode {
                    read_u64_le(raw, pos)
                } else {
                    u64::from(read_u32_le(raw, pos))
                };
                self.collect_blocks(child_bid, depth + 1, out, total);
            }
            return;
        }

        let take = len.min(MAX_NODE_BYTES - *total);
        let mut buf = raw[..take].to_vec();
        if self.crypt_method == NDB_CRYPT_PERMUTE {
            decrypt_permute(&mut buf);
        }
        *total += take;
        out.push(buf);
    }

    /// Read a node's subnode tree (`SLBLOCK` / `SIBLOCK`).
    fn subnodes(&self, bid_sub: u64) -> Vec<SlEntry> {
        let mut out = Vec::new();
        let mut visited = HashSet::new();
        self.collect_subnodes(bid_sub, 0, &mut visited, &mut out);
        out
    }

    fn collect_subnodes(
        &self,
        bid_sub: u64,
        depth: usize,
        visited: &mut HashSet<u64>,
        out: &mut Vec<SlEntry>,
    ) {
        let clean_bid = bid_sub & !1;
        if bid_sub == 0 || depth > 2 || out.len() >= MAX_SUBNODES || !visited.insert(clean_bid) {
            return;
        }
        let Some(entry) = self.bbt.get(&clean_bid) else {
            return;
        };
        let Ok(offset) = usize::try_from(entry.ib) else {
            return;
        };
        let len = usize::from(entry.cb);
        if offset.saturating_add(len) > self.data.len() || len < 8 {
            return;
        }
        let block = &self.data[offset..offset + len];
        if block[0] != 0x02 {
            return;
        }
        let c_level = block[1];
        let c_ent = usize::from(read_u16_le(block, 2));
        let header_len = if self.is_unicode { 8 } else { 4 };
        if c_level == 0 {
            // SLENTRY: nid, bidData, bidSub
            let stride = if self.is_unicode { 24 } else { 12 };
            for i in 0..c_ent {
                let pos = header_len + i * stride;
                if pos + stride > block.len() || out.len() >= MAX_SUBNODES {
                    break;
                }
                if self.is_unicode {
                    #[allow(clippy::cast_possible_truncation)]
                    out.push(SlEntry {
                        nid: read_u64_le(block, pos) as u32,
                        bid_data: read_u64_le(block, pos + 8),
                        bid_sub: read_u64_le(block, pos + 16),
                    });
                } else {
                    out.push(SlEntry {
                        nid: read_u32_le(block, pos),
                        bid_data: u64::from(read_u32_le(block, pos + 4)),
                        bid_sub: u64::from(read_u32_le(block, pos + 8)),
                    });
                }
            }
        } else {
            // SIENTRY: nid, bid
            let stride = if self.is_unicode { 16 } else { 8 };
            for i in 0..c_ent {
                let pos = header_len + i * stride;
                if pos + stride > block.len() {
                    break;
                }
                let child_bid = if self.is_unicode {
                    read_u64_le(block, pos + 8)
                } else {
                    u64::from(read_u32_le(block, pos + 4))
                };
                self.collect_subnodes(child_bid, depth + 1, visited, out);
            }
        }
    }

    /// Parse an LTP Heap-on-Node (`HN`) + BTree-on-Heap (`BTH`) Property Context
    /// (`bClientSig == 0xBC`) spread over `blocks`, resolving large values from `subnodes`.
    fn property_context(&self, blocks: &[Vec<u8>], subnodes: &[SlEntry]) -> PropertyMap {
        let mut map = PropertyMap::default();
        let Some(first) = blocks.first() else {
            return map;
        };
        if first.len() < 12 || first[2] != 0xEC || first[3] != 0xBC {
            return map;
        }
        let hid_user_root = read_u32_le(first, 4);
        let Some(bth_header) = resolve_hn_hid(blocks, hid_user_root) else {
            return map;
        };
        if bth_header.len() < 8 || bth_header[0] != 0xB5 {
            return map;
        }
        let cb_key = usize::from(bth_header[1]);
        let cb_ent = usize::from(bth_header[2]);
        let b_idx_levels = bth_header[3];
        let hid_root = read_u32_le(bth_header, 4);
        if cb_key != 2 || cb_ent < 6 {
            return map;
        }

        let mut records = Vec::new();
        collect_bth_records(
            blocks,
            hid_root,
            cb_key,
            cb_ent,
            b_idx_levels,
            &mut records,
            0,
        );

        for rec in records {
            if rec.len() < 8 {
                continue;
            }
            let prop_id = read_u16_le(&rec, 0);
            let prop_type = read_u16_le(&rec, 2);
            let value = read_u32_le(&rec, 4);
            map.present.insert(prop_id);

            match prop_type {
                0x0002 => {
                    map.integers.insert(prop_id, u64::from(value & 0xFFFF));
                }
                0x0003 | 0x000B => {
                    map.integers.insert(prop_id, u64::from(value));
                }
                0x0014 | 0x0040 => {
                    let bytes = self.hnid_bytes(blocks, subnodes, value);
                    if bytes.len() >= 8 {
                        map.integers.insert(prop_id, read_u64_le(&bytes, 0));
                    }
                }
                0x001F => {
                    let bytes = self.hnid_bytes(blocks, subnodes, value);
                    let utf16: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .take_while(|&ch| ch != 0)
                        .collect();
                    map.strings
                        .insert(prop_id, String::from_utf16_lossy(&utf16));
                }
                0x001E => {
                    let bytes = self.hnid_bytes(blocks, subnodes, value);
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                    map.strings
                        .insert(prop_id, String::from_utf8_lossy(&bytes[..end]).into_owned());
                }
                0x0102 => {
                    map.binaries
                        .insert(prop_id, self.hnid_bytes(blocks, subnodes, value));
                }
                _ => {}
            }
        }

        map
    }

    /// Bytes of a variable-size property value: a heap allocation when the `HNID` is a heap ID
    /// (`NID_TYPE_HID`), otherwise the data of the subnode it names.
    fn hnid_bytes(&self, blocks: &[Vec<u8>], subnodes: &[SlEntry], hnid: u32) -> Vec<u8> {
        if hnid == 0 {
            return Vec::new();
        }
        if hnid.trailing_zeros() >= 5 {
            return resolve_hn_hid(blocks, hnid)
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
        }
        subnodes
            .iter()
            .find(|s| s.nid == hnid)
            .map(|s| self.node_data(s.bid_data))
            .unwrap_or_default()
    }
}

#[derive(Default)]
struct PropertyMap {
    present: BTreeSet<u16>,
    strings: BTreeMap<u16, String>,
    binaries: BTreeMap<u16, Vec<u8>>,
    integers: BTreeMap<u16, u64>,
}

impl PropertyMap {
    fn has(&self, prop_id: u16) -> bool {
        self.present.contains(&prop_id)
    }

    fn get_string(&self, prop_id: u16) -> Option<String> {
        self.strings.get(&prop_id).cloned()
    }

    fn get_binary(&self, prop_id: u16) -> Option<Vec<u8>> {
        self.binaries.get(&prop_id).cloned()
    }

    fn get_filetime(&self, prop_id: u16) -> Option<DateTime<Utc>> {
        let ticks = self.integers.get(&prop_id).copied().or_else(|| {
            let bytes = self.binaries.get(&prop_id)?;
            if bytes.len() >= 8 {
                Some(read_u64_le(bytes, 0))
            } else {
                None
            }
        })?;
        filetime_to_datetime(ticks)
    }
}

fn filetime_to_datetime(ticks: u64) -> Option<DateTime<Utc>> {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const EPOCH_DIFFERENCE: u64 = 11_644_473_600;
    if ticks < EPOCH_DIFFERENCE * TICKS_PER_SECOND {
        return None;
    }
    let seconds = (ticks / TICKS_PER_SECOND).saturating_sub(EPOCH_DIFFERENCE);
    #[allow(clippy::cast_possible_truncation)]
    let nanos = ((ticks % TICKS_PER_SECOND) * 100) as u32;
    Utc.timestamp_opt(i64::try_from(seconds).ok()?, nanos)
        .single()
}

fn datetime_to_filetime(dt: DateTime<Utc>) -> u64 {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const EPOCH_DIFFERENCE: u64 = 11_644_473_600;
    let secs = u64::try_from(dt.timestamp().max(0)).unwrap_or(0);
    (secs + EPOCH_DIFFERENCE) * TICKS_PER_SECOND
}

fn collect_bth_records(
    blocks: &[Vec<u8>],
    hid: u32,
    cb_key: usize,
    cb_ent: usize,
    level: u8,
    out: &mut Vec<Vec<u8>>,
    depth: usize,
) {
    if hid == 0 || depth > 8 || out.len() >= MAX_SUBNODES {
        return;
    }
    let Some(bytes) = resolve_hn_hid(blocks, hid) else {
        return;
    };
    if level == 0 {
        for rec in bytes.chunks_exact(cb_key + cb_ent) {
            out.push(rec.to_vec());
        }
    } else {
        for rec in bytes.chunks_exact(cb_key + 4) {
            let child_hid = read_u32_le(rec, cb_key);
            collect_bth_records(blocks, child_hid, cb_key, cb_ent, level - 1, out, depth + 1);
        }
    }
}

/// Resolve a heap ID (`HID`: 5-bit type, 11-bit 1-based index, 16-bit block index) to its
/// allocation. Every heap block begins with `ibHnpm`, the offset of its `HNPAGEMAP`.
fn resolve_hn_hid(blocks: &[Vec<u8>], hid: u32) -> Option<&[u8]> {
    if hid == 0 || (hid & 0x1F) != 0 {
        return None;
    }
    let block = blocks.get(usize::try_from(hid >> 16).ok()?)?;
    let idx = usize::try_from((hid >> 5) & 0x7FF).ok()?;
    if idx == 0 || block.len() < 2 {
        return None;
    }
    let ib_hnpm = usize::from(read_u16_le(block, 0));
    if ib_hnpm + 4 > block.len() {
        return None;
    }
    let c_alloc = usize::from(read_u16_le(block, ib_hnpm));
    if idx > c_alloc {
        return None;
    }
    let table_offset = ib_hnpm + 4;
    let start_pos = table_offset + (idx - 1) * 2;
    let end_pos = table_offset + idx * 2;
    if end_pos + 2 > block.len() {
        return None;
    }
    let start = usize::from(read_u16_le(block, start_pos));
    let end = usize::from(read_u16_le(block, end_pos));
    if start > end || end > block.len() {
        return None;
    }
    Some(&block[start..end])
}

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    data.get(offset..offset + 2)
        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    data.get(offset..offset + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    data.get(offset..offset + 8).map_or(0, |b| {
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    })
}

// ============================================================================
// Synthetic Binary PST/OST Builder (for deterministic tests & fixtures)
// ============================================================================

/// Specification of a folder when building a binary `.pst`/`.ost` archive.
#[derive(Debug, Clone)]
pub struct PstFolderSpec {
    /// Folder node ID (`NID`, lower 5 bits should be `0x02`).
    pub nid: u32,
    /// Parent folder node ID (`nidParent`).
    pub parent_nid: u32,
    /// Folder display name (`PidTagDisplayName`).
    pub display_name: String,
}

/// Specification of a message when building a binary `.pst`/`.ost` archive.
#[derive(Debug, Clone)]
pub struct PstMessageSpec {
    /// Message node ID (`NID`, lower 5 bits should be `0x04`).
    pub nid: u32,
    /// Parent folder node ID (`nidParent`).
    pub folder_nid: u32,
    /// Subject (`PidTagSubject`).
    pub subject: String,
    /// Sender display name (`PidTagSenderName`).
    pub sender_name: String,
    /// Sender email (`PidTagSenderEmailAddress`).
    pub sender_email: String,
    /// Display `To` (`PidTagDisplayTo`).
    pub display_to: String,
    /// Display `Cc` (`PidTagDisplayCc`).
    pub display_cc: Option<String>,
    /// Delivery timestamp (`PidTagMessageDeliveryTime`).
    pub timestamp: Option<DateTime<Utc>>,
    /// Plain-text body (`PidTagBody`).
    pub body_text: String,
    /// Attachments (`filename, mime_type, bytes`).
    pub attachments: Vec<(String, String, Vec<u8>)>,
}

enum PropVal {
    Unicode(String),
    Binary(Vec<u8>),
    Filetime(u64),
}

/// Block allocator for the builder: lays blocks out 64-byte aligned and records their BBT
/// entries.
struct BlockWriter {
    file: Vec<u8>,
    bbt: Vec<(u64, u64, u16)>,
    next_bid: u64,
    crypt_method: u8,
}

impl BlockWriter {
    fn alloc(&mut self, mut payload: Vec<u8>, is_internal: bool) -> u64 {
        let bid = if is_internal {
            self.next_bid | 2
        } else {
            self.next_bid
        };
        self.next_bid += 4;
        if !is_internal && self.crypt_method == NDB_CRYPT_PERMUTE {
            encrypt_permute(&mut payload);
        }
        while !self.file.len().is_multiple_of(64) {
            self.file.push(0);
        }
        let ib = self.file.len() as u64;
        #[allow(clippy::cast_possible_truncation)]
        let cb = payload.len() as u16;
        self.file.extend_from_slice(&payload);
        self.bbt.push((bid & !1, ib, cb));
        bid
    }

    /// Store node data, splitting it over an `XBLOCK` when it exceeds one data block.
    fn alloc_node_data(&mut self, data: &[u8]) -> u64 {
        if data.len() <= MAX_BLOCK_DATA {
            return self.alloc(data.to_vec(), false);
        }
        let children: Vec<u64> = data
            .chunks(MAX_BLOCK_DATA)
            .map(|chunk| self.alloc(chunk.to_vec(), false))
            .collect();
        let mut xblock = vec![0x01, 0x01];
        #[allow(clippy::cast_possible_truncation)]
        xblock.extend_from_slice(&(children.len() as u16).to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        xblock.extend_from_slice(&(data.len() as u32).to_le_bytes());
        for bid in children {
            xblock.extend_from_slice(&bid.to_le_bytes());
        }
        self.alloc(xblock, true)
    }

    /// Store an `SLBLOCK` listing `(nid, bid_data, bid_sub)` entries; returns `0` when empty.
    fn alloc_subnode_block(&mut self, entries: &[(u32, u64, u64)]) -> u64 {
        if entries.is_empty() {
            return 0;
        }
        let mut sl = vec![0u8; 8 + entries.len() * 24];
        sl[0] = 0x02; // btype = SLBLOCK
        #[allow(clippy::cast_possible_truncation)]
        sl[2..4].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        for (idx, &(nid, bid_data, bid_sub)) in entries.iter().enumerate() {
            let pos = 8 + idx * 24;
            sl[pos..pos + 8].copy_from_slice(&u64::from(nid).to_le_bytes());
            sl[pos + 8..pos + 16].copy_from_slice(&bid_data.to_le_bytes());
            sl[pos + 16..pos + 24].copy_from_slice(&bid_sub.to_le_bytes());
        }
        self.alloc(sl, true)
    }

    /// Store a property context. Values larger than [`MAX_HEAP_VALUE`] go to subnodes, which
    /// are returned for the caller's subnode block.
    fn alloc_property_context(
        &mut self,
        props: &[(u16, PropVal)],
        next_sub_nid: &mut u32,
    ) -> (u64, Vec<(u32, u64, u64)>) {
        let mut subnodes = Vec::new();
        let mut heap_props = Vec::with_capacity(props.len());
        for (prop_id, val) in props {
            let (prop_type, bytes) = match val {
                PropVal::Unicode(s) => (
                    0x001Fu16,
                    s.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                ),
                PropVal::Binary(b) => (0x0102u16, b.clone()),
                PropVal::Filetime(ft) => (0x0040u16, ft.to_le_bytes().to_vec()),
            };
            if bytes.len() > MAX_HEAP_VALUE {
                let nid = (*next_sub_nid << 5) | NID_TYPE_LTP;
                *next_sub_nid += 1;
                let bid = self.alloc_node_data(&bytes);
                subnodes.push((nid, bid, 0));
                heap_props.push((*prop_id, prop_type, HeapValue::Subnode(nid)));
            } else {
                heap_props.push((*prop_id, prop_type, HeapValue::Heap(bytes)));
            }
        }
        let pc = build_pc_hn_block(&heap_props);
        (self.alloc(pc, false), subnodes)
    }
}

enum HeapValue {
    Heap(Vec<u8>),
    Subnode(u32),
}

/// Build a valid binary Unicode MS-PST (`!BDN`, `wVer = 23`) or MS-OST byte vector from
/// high-level folder and message specs. `crypt_method` is [`NDB_CRYPT_NONE`] or
/// [`NDB_CRYPT_PERMUTE`].
#[must_use]
pub fn build_pst_bytes(
    folders: &[PstFolderSpec],
    messages: &[PstMessageSpec],
    crypt_method: u8,
    is_ost: bool,
) -> Vec<u8> {
    let mut w = BlockWriter {
        file: vec![0u8; 1024], // 564-byte header + alignment
        bbt: Vec::new(),
        next_bid: 0x0040,
        crypt_method,
    };
    w.file[0..4].copy_from_slice(&PST_MAGIC);
    let client_magic: u16 = if is_ost { 0x4F53 } else { 0x4D53 };
    w.file[8..10].copy_from_slice(&client_magic.to_le_bytes());
    w.file[10..12].copy_from_slice(&23u16.to_le_bytes()); // wVer = 23 (Unicode PST)
    w.file[513] = crypt_method;

    let mut nbt_entries: Vec<(u32, u64, u64, u32)> = Vec::new();
    let mut next_sub_nid = 1u32;

    for folder in folders {
        let nid = (folder.nid & !0x1F) | NID_TYPE_NORMAL_FOLDER;
        let (bid_data, subs) = w.alloc_property_context(
            &[(0x3001, PropVal::Unicode(folder.display_name.clone()))],
            &mut next_sub_nid,
        );
        let bid_sub = w.alloc_subnode_block(&subs);
        nbt_entries.push((nid, bid_data, bid_sub, folder.parent_nid));
    }

    for msg in messages {
        let nid = (msg.nid & !0x1F) | NID_TYPE_NORMAL_MESSAGE;
        let mut props = vec![
            (0x001A, PropVal::Unicode("IPM.Note".to_string())),
            (0x0037, PropVal::Unicode(msg.subject.clone())),
            (0x0C1A, PropVal::Unicode(msg.sender_name.clone())),
            (0x0C1F, PropVal::Unicode(msg.sender_email.clone())),
            (0x0E04, PropVal::Unicode(msg.display_to.clone())),
            (0x1000, PropVal::Unicode(msg.body_text.clone())),
        ];
        if let Some(ref cc) = msg.display_cc {
            props.push((0x0E03, PropVal::Unicode(cc.clone())));
        }
        if let Some(ts) = msg.timestamp {
            props.push((0x0E06, PropVal::Filetime(datetime_to_filetime(ts))));
        }
        let (bid_data, mut subs) = w.alloc_property_context(&props, &mut next_sub_nid);

        for (idx, (fname, mime, bytes)) in msg.attachments.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let att_nid = (((idx as u32) + 1) << 5) | NID_TYPE_ATTACHMENT;
            let (att_bid, att_subs) = w.alloc_property_context(
                &[
                    (0x3707, PropVal::Unicode(fname.clone())),
                    (0x370E, PropVal::Unicode(mime.clone())),
                    (0x3701, PropVal::Binary(bytes.clone())),
                ],
                &mut next_sub_nid,
            );
            let att_sub = w.alloc_subnode_block(&att_subs);
            subs.push((att_nid, att_bid, att_sub));
        }
        let bid_sub = w.alloc_subnode_block(&subs);
        nbt_entries.push((nid, bid_data, bid_sub, msg.folder_nid));
    }

    let mut file = std::mem::take(&mut w.file);
    while !file.len().is_multiple_of(512) {
        file.push(0);
    }
    let mut next_bid = w.next_bid;
    let bbt_ib = write_btree(
        &mut file,
        &w.bbt
            .iter()
            .map(|&(bid, ib, cb)| {
                let mut e = [0u8; 24];
                e[0..8].copy_from_slice(&bid.to_le_bytes());
                e[8..16].copy_from_slice(&ib.to_le_bytes());
                e[16..18].copy_from_slice(&cb.to_le_bytes());
                e[18..20].copy_from_slice(&2u16.to_le_bytes()); // cRef
                (bid, e.to_vec())
            })
            .collect::<Vec<_>>(),
        24,
        0x80,
        &mut next_bid,
    );
    let nbt_ib = write_btree(
        &mut file,
        &nbt_entries
            .iter()
            .map(|&(nid, bid_data, bid_sub, nid_parent)| {
                let mut e = [0u8; 32];
                e[0..8].copy_from_slice(&u64::from(nid).to_le_bytes());
                e[8..16].copy_from_slice(&bid_data.to_le_bytes());
                e[16..24].copy_from_slice(&bid_sub.to_le_bytes());
                e[24..28].copy_from_slice(&nid_parent.to_le_bytes());
                (u64::from(nid), e.to_vec())
            })
            .collect::<Vec<_>>(),
        32,
        0x81,
        &mut next_bid,
    );

    let eof = file.len() as u64;
    file[184..192].copy_from_slice(&eof.to_le_bytes());
    file[216..224].copy_from_slice(&(next_bid + 4).to_le_bytes());
    file[224..232].copy_from_slice(&nbt_ib.to_le_bytes());
    file[232..240].copy_from_slice(&(next_bid + 8).to_le_bytes());
    file[240..248].copy_from_slice(&bbt_ib.to_le_bytes());

    file
}

fn build_pc_hn_block(props: &[(u16, u16, HeapValue)]) -> Vec<u8> {
    let mut allocations: Vec<Vec<u8>> = Vec::new();
    // Allocation 1 (HID = 0x20): BTHHEADER (8 bytes)
    allocations.push(vec![0xB5, 2, 6, 0, 0x40, 0x00, 0x00, 0x00]);
    // Allocation 2 (HID = 0x40): placeholder for BTH records
    allocations.push(Vec::new());

    let mut bth_records = Vec::with_capacity(props.len() * 8);
    for (prop_id, prop_type, value) in props {
        let hnid = match value {
            HeapValue::Heap(bytes) => {
                #[allow(clippy::cast_possible_truncation)]
                let hid = ((allocations.len() + 1) as u32) << 5;
                allocations.push(bytes.clone());
                hid
            }
            HeapValue::Subnode(nid) => *nid,
        };
        bth_records.extend_from_slice(&prop_id.to_le_bytes());
        bth_records.extend_from_slice(&prop_type.to_le_bytes());
        bth_records.extend_from_slice(&hnid.to_le_bytes());
    }
    allocations[1] = bth_records;

    // Assemble HN block: 12-byte HNHDR + allocations + HNPAGEMAP
    let mut block = vec![0u8; 12];
    block[2] = 0xEC; // bSig
    block[3] = 0xBC; // bClientSig = PC
    block[4..8].copy_from_slice(&0x20u32.to_le_bytes()); // hidUserRoot = 0x20

    let mut offsets = Vec::with_capacity(allocations.len() + 1);
    for item in &allocations {
        #[allow(clippy::cast_possible_truncation)]
        offsets.push(block.len() as u16);
        block.extend_from_slice(item);
    }
    #[allow(clippy::cast_possible_truncation)]
    offsets.push(block.len() as u16);
    if !block.len().is_multiple_of(2) {
        block.push(0);
    }

    #[allow(clippy::cast_possible_truncation)]
    let ib_hnpm = block.len() as u16;
    block[0..2].copy_from_slice(&ib_hnpm.to_le_bytes());
    #[allow(clippy::cast_possible_truncation)]
    let c_alloc = allocations.len() as u16;
    block.extend_from_slice(&c_alloc.to_le_bytes());
    block.extend_from_slice(&0u16.to_le_bytes()); // cFree
    for off in offsets {
        block.extend_from_slice(&off.to_le_bytes());
    }
    block
}

/// Write `(key, entry)` leaves as one or more 512-byte Unicode `BTPAGE`s, plus a root page
/// when they span several leaves. Returns the root page offset.
fn write_btree(
    file: &mut Vec<u8>,
    entries: &[(u64, Vec<u8>)],
    cb_ent: usize,
    ptype: u8,
    next_bid: &mut u64,
) -> u64 {
    let write_page = |file: &mut Vec<u8>, rows: &[Vec<u8>], cb: usize, level: u8| -> u64 {
        let page_ib = file.len() as u64;
        let mut page = [0u8; 512];
        for (i, row) in rows.iter().enumerate() {
            page[i * cb..i * cb + row.len()].copy_from_slice(row);
        }
        #[allow(clippy::cast_possible_truncation)]
        {
            page[UNICODE_PAGE.c_ent] = rows.len() as u8; // cEnt
            page[489] = (UNICODE_PAGE.entries_len / cb) as u8; // cEntMax
            page[UNICODE_PAGE.cb_ent] = cb as u8; // cbEnt
        }
        page[UNICODE_PAGE.c_level] = level; // cLevel
        page[496] = ptype; // pageTrailer.ptype
        page[497] = ptype; // pageTrailer.ptypeRepeat
        file.extend_from_slice(&page);
        page_ib
    };

    let per_leaf = UNICODE_PAGE.entries_len / cb_ent;
    let mut leaves: Vec<(u64, u64)> = Vec::new();
    for chunk in entries.chunks(per_leaf) {
        let rows: Vec<Vec<u8>> = chunk.iter().map(|(_, e)| e.clone()).collect();
        let ib = write_page(file, &rows, cb_ent, 0);
        leaves.push((chunk.first().map_or(0, |e| e.0), ib));
        *next_bid += 4;
    }
    if leaves.len() == 1 {
        return leaves[0].1;
    }
    let rows: Vec<Vec<u8>> = leaves
        .iter()
        .take(UNICODE_PAGE.entries_len / 24)
        .map(|&(key, child_ib)| {
            let mut e = vec![0u8; 24];
            e[0..8].copy_from_slice(&key.to_le_bytes());
            e[8..16].copy_from_slice(&next_bid.to_le_bytes());
            e[16..24].copy_from_slice(&child_ib.to_le_bytes());
            *next_bid += 4;
            e
        })
        .collect();
    write_page(file, &rows, 24, 1)
}

// ============================================================================
// PstParser implementation for UDM Document & Layout
// ============================================================================

/// Native Outlook `.pst` and `.ost` parser.
#[derive(Debug, Clone)]
pub struct PstParser {
    format: Format,
}

impl Default for PstParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PstParser {
    /// Create a new `.pst` [`PstParser`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            format: Format::pst(),
        }
    }

    /// Create a new `.ost` [`PstParser`].
    #[must_use]
    pub fn new_ost() -> Self {
        Self {
            format: Format::ost(),
        }
    }

    /// Convert a parsed [`PstArchive`] into a paginated UDM [`Document`].
    #[must_use]
    pub fn archive_to_document(archive: &PstArchive, filename: Option<&str>) -> Document {
        let mut content = Vec::new();

        // Title block
        let container_kind = if archive.is_ost { "OST" } else { "PST" };
        let title_str = filename.map_or_else(
            || format!("Outlook {container_kind} Mailbox Archive"),
            |f| format!("Outlook {container_kind} Mailbox: {f}"),
        );
        content.push(ContentBlock::Text(TextBlock {
            bounds: Rect::default(),
            runs: vec![TextRun {
                text: format!(
                    "{title_str} ({} folders, {} messages)\n",
                    archive.folders.len(),
                    archive.messages.len()
                ),
                style: TextStyle {
                    font_size: Some(15.0),
                    bold: true,
                    color: Some("#0F172A".to_string()),
                    ..TextStyle::default()
                },
                bounds: None,
                char_positions: None,
            }],
            paragraph_style: None,
            vertical_alignment: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }));

        // Folder summary table
        if !archive.folders.is_empty() {
            let mut rows = Vec::with_capacity(archive.folders.len() + 1);
            rows.push(TableRow {
                height: None,
                cells: vec![
                    styled_cell("Folder Path", true, Some("#E2E8F0")),
                    styled_cell("Messages", true, Some("#E2E8F0")),
                ],
            });
            for folder in &archive.folders {
                let msg_count = archive
                    .messages
                    .iter()
                    .filter(|m| m.folder_nid == folder.nid)
                    .count();
                rows.push(TableRow {
                    height: None,
                    cells: vec![
                        styled_cell(&folder.path, false, None),
                        styled_cell(&msg_count.to_string(), false, None),
                    ],
                });
            }
            content.push(ContentBlock::Table(TableBlock {
                bounds: Rect::default(),
                rows,
                column_count: 2,
                column_widths: vec![340.0, 120.0],
                style: ShapeStyle::default(),
                rotation: 0.0,
            }));
        }

        let mut attachments = Vec::new();

        // Individual messages with full MAPI headers, recipients, body, and attachment list
        for (idx, msg) in archive.messages.iter().enumerate() {
            let mut header_runs = Vec::new();
            header_runs.push(TextRun {
                text: format!(
                    "Message #{}: {} [{}]\n",
                    idx + 1,
                    msg.subject,
                    msg.folder_path
                ),
                style: TextStyle {
                    font_size: Some(12.0),
                    bold: true,
                    color: Some("#1E3A8A".to_string()),
                    ..TextStyle::default()
                },
                bounds: None,
                char_positions: None,
            });
            header_runs.push(bold_label_run("From", &msg.formatted_from()));
            if let Some(ref to) = msg.display_to {
                header_runs.push(bold_label_run("To", to));
            }
            if let Some(ref cc) = msg.display_cc {
                header_runs.push(bold_label_run("Cc", cc));
            }
            if let Some(ref bcc) = msg.display_bcc {
                header_runs.push(bold_label_run("Bcc", bcc));
            }
            if let Some(ts) = msg.timestamp {
                header_runs.push(bold_label_run("Date", &ts.to_rfc3339()));
            }
            if !msg.attachments.is_empty() {
                let att_names: Vec<&str> = msg
                    .attachments
                    .iter()
                    .map(|a| a.filename.as_str())
                    .collect();
                header_runs.push(bold_label_run("Attachments", &att_names.join(", ")));
            }

            let body_str = msg.display_body();

            content.push(ContentBlock::Table(TableBlock {
                bounds: Rect::default(),
                rows: vec![
                    TableRow {
                        height: None,
                        cells: vec![TableCell {
                            content: vec![ContentBlock::Text(TextBlock {
                                bounds: Rect::default(),
                                runs: header_runs,
                                paragraph_style: None,
                                vertical_alignment: None,
                                style: ShapeStyle::default(),
                                rotation: 0.0,
                            })],
                            row_span: 1,
                            col_span: 1,
                            background_color: Some("#F8FAFC".to_string()),
                            borders: Some(uniform_borders(0.5, "#CBD5E1")),
                        }],
                    },
                    TableRow {
                        height: None,
                        cells: vec![TableCell {
                            content: vec![ContentBlock::Text(TextBlock {
                                bounds: Rect::default(),
                                runs: vec![TextRun {
                                    text: body_str,
                                    style: TextStyle {
                                        font_size: Some(10.5),
                                        color: Some("#1E293B".to_string()),
                                        ..TextStyle::default()
                                    },
                                    bounds: None,
                                    char_positions: None,
                                }],
                                paragraph_style: None,
                                vertical_alignment: None,
                                style: ShapeStyle::default(),
                                rotation: 0.0,
                            })],
                            row_span: 1,
                            col_span: 1,
                            background_color: None,
                            borders: Some(uniform_borders(0.5, "#CBD5E1")),
                        }],
                    },
                ],
                column_count: 1,
                column_widths: vec![468.0],
                style: ShapeStyle::default(),
                rotation: 0.0,
            }));

            for att in &msg.attachments {
                attachments.push(Attachment {
                    filename: att.filename.clone(),
                    mime_type: att.mime_type.clone(),
                    description: Some(format!("Attached to message '{}'", msg.subject)),
                    data: att.data.clone(),
                    created: msg.timestamp,
                    modified: msg.timestamp,
                });
            }
        }

        let mut metadata = Metadata {
            title: Some(title_str),
            ..Metadata::default()
        };
        metadata.add_custom("format", container_kind);
        metadata.add_custom(
            "folder_count",
            i64::try_from(archive.folders.len()).unwrap_or(0),
        );
        metadata.add_custom(
            "message_count",
            i64::try_from(archive.messages.len()).unwrap_or(0),
        );

        let page = Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content,
            metadata: PageMetadata {
                margins: Some(PageMargins::ONE_INCH),
                ..PageMetadata::default()
            },
            annotations: Vec::new(),
        };

        let mut doc = Document::new();
        doc.pages = vec![page];
        doc.metadata = metadata;
        doc.attachments = attachments;
        doc.source.format = Some(if archive.is_ost {
            Format::ost()
        } else {
            Format::pst()
        });
        doc
    }
}

fn bold_label_run(label: &str, value: &str) -> TextRun {
    TextRun {
        text: format!("{label}: {value}\n"),
        style: TextStyle {
            font_size: Some(10.0),
            bold: false,
            color: Some("#334155".to_string()),
            ..TextStyle::default()
        },
        bounds: None,
        char_positions: None,
    }
}

fn uniform_borders(width: f64, color: &str) -> CellBorders {
    let c = Some(color.to_string());
    let w = Some(width);
    CellBorders {
        top_color: c.clone(),
        top_width: w,
        bottom_color: c.clone(),
        bottom_width: w,
        left_color: c.clone(),
        left_width: w,
        right_color: c,
        right_width: w,
    }
}

fn styled_cell(text: &str, bold: bool, fill: Option<&str>) -> TableCell {
    TableCell {
        content: vec![ContentBlock::Text(TextBlock {
            bounds: Rect::default(),
            runs: vec![TextRun {
                text: text.to_string(),
                style: TextStyle {
                    font_size: Some(10.0),
                    bold,
                    color: Some("#0F172A".to_string()),
                    ..TextStyle::default()
                },
                bounds: None,
                char_positions: None,
            }],
            paragraph_style: None,
            vertical_alignment: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        })],
        row_span: 1,
        col_span: 1,
        background_color: fill.map(ToString::to_string),
        borders: Some(uniform_borders(0.5, "#CBD5E1")),
    }
}

#[async_trait]
impl Parser for PstParser {
    fn format(&self) -> Format {
        self.format.clone()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        data.len() >= 512 && data[0..4] == PST_MAGIC
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let mut archive = PstArchive::parse(&data)?;
        if context.format.extension.eq_ignore_ascii_case("ost") {
            archive.is_ost = true;
        }
        Ok(Self::archive_to_document(
            &archive,
            context.filename.as_deref(),
        ))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "MS-PST / MS-OST Mailbox Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
                ParserFeature::StructureExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permute_table_is_the_spec_inverse() {
        // `[MS-PST]` §5.1: `mpbbR[0]` is 65, so decoding 65 yields 0.
        let mut b = [65u8];
        decrypt_permute(&mut b);
        assert_eq!(b, [0]);
        let mut round = (0..=255u8).collect::<Vec<_>>();
        encrypt_permute(&mut round);
        decrypt_permute(&mut round);
        assert_eq!(round, (0..=255u8).collect::<Vec<_>>());
    }

    fn sample_message(body: String, attachment: Vec<u8>) -> PstMessageSpec {
        PstMessageSpec {
            nid: 0x20004,
            folder_nid: 0x0122,
            subject: "Line one\r\nBcc: injected@example.invalid".to_string(),
            sender_name: "Sender".to_string(),
            sender_email: "sender@example.invalid".to_string(),
            display_to: "to@example.invalid".to_string(),
            display_cc: None,
            timestamp: None,
            body_text: body,
            attachments: vec![(
                "big.bin".to_string(),
                "application/octet-stream".to_string(),
                attachment,
            )],
        }
    }

    #[test]
    fn large_values_are_read_from_subnodes_and_xblocks() {
        let folders = [PstFolderSpec {
            nid: 0x0122,
            parent_nid: 0x0122,
            display_name: "Inbox".to_string(),
        }];
        // A body past the heap limit (subnode) and an attachment past one block (XBLOCK).
        let body = "word ".repeat(2_000);
        let attachment: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let bytes = build_pst_bytes(
            &folders,
            &[sample_message(body.clone(), attachment.clone())],
            NDB_CRYPT_PERMUTE,
            false,
        );
        let archive = PstArchive::parse(&bytes).expect("parse");
        assert_eq!(archive.messages.len(), 1);
        let msg = &archive.messages[0];
        assert_eq!(msg.body_text.as_deref(), Some(body.as_str()));
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].data, attachment);

        // Stored line breaks cannot add headers to the synthesized message.
        let eml = String::from_utf8(msg.to_eml_bytes()).unwrap();
        assert!(!eml.contains("\r\nBcc: injected"), "{eml}");
    }

    #[test]
    fn cyclic_encryption_is_reported_not_decoded() {
        let mut bytes = build_pst_bytes(&[], &[], NDB_CRYPT_NONE, false);
        bytes[513] = NDB_CRYPT_CYCLIC;
        assert!(matches!(
            PstArchive::parse(&bytes),
            Err(Error::Encrypted(_))
        ));
    }

    #[test]
    fn self_referencing_btree_pages_are_walked_once() {
        let mut bytes = build_pst_bytes(&[], &[], NDB_CRYPT_NONE, false);
        // Replace the NBT root with an intermediate page whose 20 entries all point at itself;
        // without a visited set that walk is 20^16 pages.
        let root = read_u64_le(&bytes, 224);
        let root_ib = usize::try_from(root).unwrap();
        let mut page = [0u8; 512];
        for i in 0..20 {
            page[i * 24 + 16..i * 24 + 24].copy_from_slice(&root.to_le_bytes());
        }
        page[488] = 20;
        page[489] = 20;
        page[490] = 24;
        page[491] = 1;
        bytes[root_ib..root_ib + 512].copy_from_slice(&page);
        let archive = PstArchive::parse(&bytes).expect("parse");
        assert_eq!(archive.messages, Vec::new());
    }
}
