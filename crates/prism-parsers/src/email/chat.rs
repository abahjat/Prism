// SPDX-License-Identifier: AGPL-3.0-only
//! eDiscovery Short-Message & Chat Format Parsers (`RSMF`, Slack, Microsoft Teams, `WhatsApp`).
//!
//! Normalizes short-message exports into a unified [`ChatTranscript`] model and renders
//! them into paginated UDM [`Document`] bubble timelines with participant rosters,
//! conversation headers, inline image attachments, reactions, and forensic markers
//! for edited and deleted messages.

#![allow(clippy::too_many_lines)]

use crate::limits::read_bounded;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use mail_parser::MessageParser;
use prism_core::{
    document::{
        Attachment, CellBorders, ContentBlock, Dimensions, Document, ImageBlock, ImageResource,
        Page, PageMargins, PageMetadata, Rect, ShapeStyle, TableBlock, TableCell, TableRow,
        TextBlock, TextRun, TextStyle,
    },
    error::{Error, Result},
    format::Format,
    metadata::Metadata,
    parser::{ParseContext, Parser, ParserFeature, ParserMetadata},
    review::{Finding, FindingKind, Location, Severity},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use zip::ZipArchive;

/// Source platform of a short-message transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatPlatform {
    /// Relativity Short Message Format (`.rsmf`).
    Rsmf,
    /// Slack JSON workspace/channel export.
    Slack,
    /// Microsoft Teams chat export.
    Teams,
    /// `WhatsApp` text chat archive.
    WhatsApp,
}

impl ChatPlatform {
    /// Short uppercase format identifier stored in `Document::metadata.custom["format"]`.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Rsmf => "RSMF",
            Self::Slack => "SLACK",
            Self::Teams => "TEAMS",
            Self::WhatsApp => "WHATSAPP",
        }
    }

    /// Human-readable platform label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rsmf => "Relativity Short Message Format (RSMF)",
            Self::Slack => "Slack Chat Export",
            Self::Teams => "Microsoft Teams Chat",
            Self::WhatsApp => "WhatsApp Chat Transcript",
        }
    }
}

/// Participant in a short-message conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatParticipant {
    /// Unique participant ID (e.g. `"P1"`, `"U012AB3CD"`).
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Optional email or handle.
    pub email: Option<String>,
}

/// Conversation / channel metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatConversation {
    /// Unique conversation ID.
    pub id: String,
    /// Display name (e.g. `"#project-alpha"`, `"Executive Direct Chat"`).
    pub display_name: String,
    /// Platform string (e.g. `"slack"`, `"teams"`, `"sms"`, `"whatsapp"`).
    pub platform: String,
    /// Conversation type (`"channel"` or `"direct"`).
    pub conv_type: String,
    /// Participant IDs in this conversation.
    pub participants: Vec<String>,
}

/// Emoji reaction on a chat event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatReaction {
    /// Reaction emoji or short name.
    pub value: String,
    /// Count of reactions.
    pub count: usize,
    /// Participant display names or IDs who reacted.
    pub participants: Vec<String>,
}

/// Recorded edit history entry on a chat event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEdit {
    /// Editor participant ID or name.
    pub participant: Option<String>,
    /// Timestamp of the edit.
    pub timestamp: Option<String>,
    /// Previous text before the edit.
    pub previous: Option<String>,
    /// New text after the edit.
    pub new_text: Option<String>,
}

/// Attachment referenced by a chat event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatAttachment {
    /// Attachment ID / path inside container (e.g. `"chart.png"`).
    pub id: String,
    /// Display filename.
    pub display_name: String,
    /// Optional MIME type.
    pub mime_type: Option<String>,
    /// Optional byte size.
    pub size: Option<u64>,
}

/// A single message, join/leave, or system event in a chat transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEvent {
    /// Event ID (`"m1"`, `"1700000000.000100"`, etc.).
    pub id: String,
    /// Conversation ID this event belongs to.
    pub conversation_id: Option<String>,
    /// Sender participant ID.
    pub participant_id: Option<String>,
    /// Resolved sender display name.
    pub participant_display: String,
    /// ISO-8601 or formatted timestamp string.
    pub timestamp: Option<String>,
    /// Message text body.
    pub body: String,
    /// Event type (`"message"`, `"join"`, `"leave"`, `"disclaimer"`).
    pub event_type: String,
    /// Parent message ID for threaded replies.
    pub parent_id: Option<String>,
    /// Reactions attached to this message.
    pub reactions: Vec<ChatReaction>,
    /// Edit history entries.
    pub edits: Vec<ChatEdit>,
    /// True if this message was deleted / tombstoned.
    pub deleted: bool,
    /// Attachments referenced by this message.
    pub attachments: Vec<ChatAttachment>,
}

/// Normalized short-message transcript across RSMF, Slack, Teams, and `WhatsApp`.
#[derive(Debug, Clone)]
pub struct ChatTranscript {
    /// Source chat format/platform.
    pub platform: ChatPlatform,
    /// Optional RSMF version string (e.g. `"2.0.0"`).
    pub version: Option<String>,
    /// Roster of participants.
    pub participants: Vec<ChatParticipant>,
    /// Conversations in the transcript.
    pub conversations: Vec<ChatConversation>,
    /// Ordered timeline of chat events.
    pub events: Vec<ChatEvent>,
    /// Raw attachment files bundled in the container (filename -> bytes).
    pub media_files: BTreeMap<String, Vec<u8>>,
}

impl ChatTranscript {
    /// Parse a [`ChatTranscript`] from raw bytes based on the detected [`ChatPlatform`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ParseError`] if the payload cannot be parsed.
    pub fn parse(data: &[u8], platform: ChatPlatform) -> Result<Self> {
        match platform {
            ChatPlatform::Rsmf => parse_rsmf(data),
            ChatPlatform::Slack => parse_slack_json(data),
            ChatPlatform::Teams => parse_teams_json(data),
            ChatPlatform::WhatsApp => parse_whatsapp_text(data),
        }
    }

    /// Collect forensic [`Finding`] entries for deleted and edited messages in this transcript.
    #[must_use]
    pub fn collect_forensic_findings(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (idx, ev) in self.events.iter().enumerate() {
            let loc = Location::PartPath {
                path: format!("events[{idx}]/{}", ev.id),
            };
            if ev.deleted {
                findings.push(Finding::new(
                    FindingKind::TrackedDeletion,
                    Severity::High,
                    loc.clone(),
                    None,
                    Some(format!(
                        "Deleted chat message from {}: {}",
                        ev.participant_display,
                        if ev.body.is_empty() {
                            "[deleted message content]"
                        } else {
                            &ev.body
                        }
                    )),
                    serde_json::json!({
                        "event_id": ev.id,
                        "participant": ev.participant_display,
                        "timestamp": ev.timestamp,
                        "deleted": true,
                    }),
                ));
            }
            if !ev.edits.is_empty() {
                for edit in &ev.edits {
                    let prev_info = edit
                        .previous
                        .as_deref()
                        .map_or_else(String::new, |p| format!(" (previous: \"{p}\")"));
                    findings.push(Finding::new(
                        FindingKind::TrackedInsertion,
                        Severity::Notable,
                        loc.clone(),
                        None,
                        Some(format!(
                            "Edited chat message from {}: \"{}\"{}",
                            ev.participant_display, ev.body, prev_info
                        )),
                        serde_json::json!({
                            "event_id": ev.id,
                            "participant": ev.participant_display,
                            "timestamp": edit.timestamp.as_ref().or(ev.timestamp.as_ref()),
                            "previous_body": edit.previous,
                            "new_body": ev.body,
                        }),
                    ));
                }
            }
        }
        findings
    }

    /// Convert this [`ChatTranscript`] into a rich, paginated UDM [`Document`] with
    /// participant roster, conversation banners, styled message bubbles, reactions,
    /// forensic `[EDITED]` / `[DELETED]` badges, and embedded media attachments.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn to_document(&self, filename: Option<&str>) -> Document {
        let mut content = Vec::new();
        let mut doc = Document::new();

        let deleted_count = self.events.iter().filter(|e| e.deleted).count();
        let edited_count = self.events.iter().filter(|e| !e.edits.is_empty()).count();

        let title_str = filename.map_or_else(
            || self.platform.label().to_string(),
            |f| format!("{}: {f}", self.platform.label()),
        );

        // Header banner
        content.push(ContentBlock::Text(TextBlock {
            bounds: Rect::default(),
            runs: vec![
                TextRun {
                    text: format!("{title_str}\n"),
                    style: TextStyle {
                        font_size: Some(15.0),
                        bold: true,
                        color: Some("#0F172A".to_string()),
                        ..TextStyle::default()
                    },
                    bounds: None,
                    char_positions: None,
                },
                TextRun {
                    text: format!(
                        "Participants: {}  |  Conversations: {}  |  Messages: {}  |  Edited: {}  |  Deleted: {}\n",
                        self.participants.len(),
                        self.conversations.len().max(1),
                        self.events.len(),
                        edited_count,
                        deleted_count
                    ),
                    style: TextStyle {
                        font_size: Some(10.0),
                        color: Some("#475569".to_string()),
                        ..TextStyle::default()
                    },
                    bounds: None,
                    char_positions: None,
                },
            ],
            paragraph_style: None,
            vertical_alignment: None,
            style: ShapeStyle::default(),
            rotation: 0.0,
        }));

        // Participant Roster Table
        if !self.participants.is_empty() {
            let mut rows = Vec::with_capacity(self.participants.len() + 1);
            rows.push(TableRow {
                height: None,
                cells: vec![
                    roster_cell("Participant ID", true, Some("#E2E8F0")),
                    roster_cell("Display Name", true, Some("#E2E8F0")),
                    roster_cell("Email / Handle", true, Some("#E2E8F0")),
                ],
            });
            for p in &self.participants {
                rows.push(TableRow {
                    height: None,
                    cells: vec![
                        roster_cell(&p.id, false, None),
                        roster_cell(&p.display_name, false, None),
                        roster_cell(p.email.as_deref().unwrap_or("-"), false, None),
                    ],
                });
            }
            content.push(ContentBlock::Table(TableBlock {
                bounds: Rect::default(),
                rows,
                column_count: 3,
                column_widths: vec![120.0, 180.0, 168.0],
                style: ShapeStyle::default(),
                rotation: 0.0,
            }));
        }

        // Map conversations by ID for banners
        let conv_map: BTreeMap<&str, &ChatConversation> = self
            .conversations
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect();

        // Register inline image resources from `media_files`
        let mut image_resource_ids: BTreeMap<String, (String, u32, u32)> = BTreeMap::new();
        for (idx, (name, bytes)) in self.media_files.iter().enumerate() {
            let mime = guess_mime_from_filename(name);
            doc.attachments.push(Attachment {
                filename: name.clone(),
                mime_type: Some(mime.clone()),
                description: Some("Chat media attachment".to_string()),
                data: bytes.clone(),
                created: None,
                modified: None,
            });
            if mime.starts_with("image/")
                && let Some((w, h)) = probe_image_dimensions(bytes)
            {
                let res_id = format!("chat_img_{idx}");
                let w = w.max(1);
                let h = h.max(1);
                doc.resources.images.push(ImageResource {
                    id: res_id.clone(),
                    mime_type: mime,
                    url: None,
                    width: w,
                    height: h,
                    data: Some(bytes.clone()),
                });
                image_resource_ids.insert(name.clone(), (res_id, w, h));
            }
        }

        let first_participant_id = self.participants.first().map(|p| p.id.as_str());
        let mut current_conv: Option<&str> = None;

        for ev in &self.events {
            if let Some(cid) = ev.conversation_id.as_deref()
                && current_conv != Some(cid)
            {
                current_conv = Some(cid);
                let banner_text = if let Some(conv) = conv_map.get(cid) {
                    format!(
                        "Conversation: {} ({} | {})\n",
                        conv.display_name, conv.platform, conv.conv_type
                    )
                } else {
                    format!("Conversation: {cid}\n")
                };
                content.push(ContentBlock::Text(TextBlock {
                    bounds: Rect::default(),
                    runs: vec![TextRun {
                        text: banner_text,
                        style: TextStyle {
                            font_size: Some(11.5),
                            bold: true,
                            color: Some("#1E40AF".to_string()),
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
            }

            let is_primary_sender = first_participant_id.is_some()
                && ev.participant_id.as_deref() == first_participant_id;

            let (fill_color, border_color) = if ev.deleted {
                ("#FEF2F2", "#EF4444")
            } else if !ev.edits.is_empty() {
                ("#FFFBEB", "#F59E0B")
            } else if is_primary_sender {
                ("#EFF6FF", "#93C5FD")
            } else {
                ("#F8FAFC", "#CBD5E1")
            };

            let mut bubble_runs = Vec::new();
            let mut meta_badges = Vec::new();
            if let Some(ref parent) = ev.parent_id {
                meta_badges.push(format!("[Reply to {parent}]"));
            }
            if !ev.edits.is_empty() {
                meta_badges.push("[EDITED]".to_string());
            }
            if ev.deleted {
                meta_badges.push("[DELETED]".to_string());
            }
            let badge_str = if meta_badges.is_empty() {
                String::new()
            } else {
                format!("  {}", meta_badges.join(" "))
            };
            let ts_str = ev
                .timestamp
                .as_deref()
                .map_or_else(String::new, |t| format!("  •  {t}"));

            bubble_runs.push(TextRun {
                text: format!("{}{}{}\n", ev.participant_display, ts_str, badge_str),
                style: TextStyle {
                    font_size: Some(9.5),
                    bold: true,
                    color: Some(if ev.deleted {
                        "#991B1B".to_string()
                    } else {
                        "#1E3A8A".to_string()
                    }),
                    ..TextStyle::default()
                },
                bounds: None,
                char_positions: None,
            });

            let body_display = if ev.deleted && ev.body.is_empty() {
                "[Message deleted by participant]".to_string()
            } else {
                ev.body.clone()
            };
            bubble_runs.push(TextRun {
                text: format!("{body_display}\n"),
                style: TextStyle {
                    font_size: Some(10.5),
                    italic: ev.deleted,
                    strikethrough: ev.deleted,
                    color: Some("#0F172A".to_string()),
                    ..TextStyle::default()
                },
                bounds: None,
                char_positions: None,
            });

            // Include previous edit history in forensic view
            for edit in &ev.edits {
                if let Some(ref prev) = edit.previous {
                    bubble_runs.push(TextRun {
                        text: format!("Prior version: \"{prev}\"\n"),
                        style: TextStyle {
                            font_size: Some(9.0),
                            italic: true,
                            color: Some("#92400E".to_string()),
                            ..TextStyle::default()
                        },
                        bounds: None,
                        char_positions: None,
                    });
                }
            }

            if !ev.reactions.is_empty() {
                let react_parts: Vec<String> = ev
                    .reactions
                    .iter()
                    .map(|r| format!("{} ({})", r.value, r.count))
                    .collect();
                bubble_runs.push(TextRun {
                    text: format!("Reactions: {}\n", react_parts.join("  ")),
                    style: TextStyle {
                        font_size: Some(9.0),
                        color: Some("#475569".to_string()),
                        ..TextStyle::default()
                    },
                    bounds: None,
                    char_positions: None,
                });
            }

            if !ev.attachments.is_empty() {
                let att_parts: Vec<String> = ev
                    .attachments
                    .iter()
                    .map(|a| a.display_name.clone())
                    .collect();
                bubble_runs.push(TextRun {
                    text: format!("Attachment: {}\n", att_parts.join(", ")),
                    style: TextStyle {
                        font_size: Some(9.0),
                        bold: true,
                        color: Some("#0369A1".to_string()),
                        ..TextStyle::default()
                    },
                    bounds: None,
                    char_positions: None,
                });
            }

            let bubble_cell = TableCell {
                content: vec![ContentBlock::Text(TextBlock {
                    bounds: Rect::default(),
                    runs: bubble_runs,
                    paragraph_style: None,
                    vertical_alignment: None,
                    style: ShapeStyle::default(),
                    rotation: 0.0,
                })],
                row_span: 1,
                col_span: 1,
                background_color: Some(fill_color.to_string()),
                borders: Some(uniform_borders(0.75, border_color)),
            };

            content.push(ContentBlock::Table(TableBlock {
                bounds: Rect::default(),
                rows: vec![TableRow {
                    height: None,
                    cells: vec![bubble_cell],
                }],
                column_count: 1,
                column_widths: vec![468.0],
                style: ShapeStyle::default(),
                rotation: 0.0,
            }));

            // Render inline image attachments beneath the bubble if present
            for att in &ev.attachments {
                if let Some((res_id, w, h)) = image_resource_ids
                    .get(&att.id)
                    .or_else(|| image_resource_ids.get(&att.display_name))
                {
                    let scale = (240.0 / f64::from(*w)).min(1.0);
                    content.push(ContentBlock::Image(ImageBlock {
                        bounds: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: (f64::from(*w) * scale).max(48.0),
                            height: (f64::from(*h) * scale).max(48.0),
                        },
                        resource_id: res_id.clone(),
                        alt_text: Some(att.display_name.clone()),
                        format: None,
                        original_size: None,
                        style: ShapeStyle::default(),
                        rotation: 0.0,
                    }));
                }
            }
        }

        let mut metadata = Metadata {
            title: Some(title_str),
            ..Metadata::default()
        };
        metadata.add_custom("format", self.platform.tag());
        if let Some(ref ver) = self.version {
            metadata.add_custom("rsmf_version", ver.clone());
        }
        metadata.add_custom(
            "participant_count",
            i64::try_from(self.participants.len()).unwrap_or(0),
        );
        metadata.add_custom("event_count", i64::try_from(self.events.len()).unwrap_or(0));
        metadata.add_custom(
            "deleted_message_count",
            i64::try_from(deleted_count).unwrap_or(0),
        );
        metadata.add_custom(
            "edited_message_count",
            i64::try_from(edited_count).unwrap_or(0),
        );

        doc.pages = vec![Page {
            number: 1,
            dimensions: Dimensions::LETTER,
            content,
            metadata: PageMetadata {
                margins: Some(PageMargins::ONE_INCH),
                ..PageMetadata::default()
            },
            annotations: Vec::new(),
        }];
        doc.metadata = metadata;
        doc.source.format = Some(match self.platform {
            ChatPlatform::Rsmf => Format::rsmf(),
            ChatPlatform::Slack => Format::slack_chat(),
            ChatPlatform::Teams => Format::teams_chat(),
            ChatPlatform::WhatsApp => Format::whatsapp_chat(),
        });
        doc
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

fn roster_cell(text: &str, bold: bool, fill: Option<&str>) -> TableCell {
    TableCell {
        content: vec![ContentBlock::Text(TextBlock {
            bounds: Rect::default(),
            runs: vec![TextRun {
                text: text.to_string(),
                style: TextStyle {
                    font_size: Some(9.5),
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

/// Read an image's dimensions from its header without decoding the pixels.
fn probe_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn guess_mime_from_filename(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string()
}

// ============================================================================
// RSMF (Relativity Short Message Format) Parser
// ============================================================================

/// Extract the inner RSMF ZIP bytes from either a direct ZIP archive or an RFC 5322 `.eml` wrapper.
#[must_use]
pub fn extract_rsmf_zip_bytes(data: &[u8]) -> Option<Vec<u8>> {
    if data.starts_with(b"PK\x03\x04") {
        return Some(data.to_vec());
    }
    if let Some(message) = MessageParser::default().parse(data) {
        for part in &message.parts {
            let bytes = part.contents();
            if bytes.starts_with(b"PK\x03\x04") {
                return Some(bytes.to_vec());
            }
        }
    }
    None
}

/// Most bytes of RSMF media kept in memory; later files are listed but not loaded.
const MAX_RSMF_MEDIA_BYTES: usize = 256 * 1024 * 1024;

fn parse_rsmf(data: &[u8]) -> Result<ChatTranscript> {
    let mut media_files = BTreeMap::new();
    let mut media_bytes = 0usize;
    let manifest_bytes = if let Some(zip_bytes) = extract_rsmf_zip_bytes(data) {
        let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
            .map_err(|e| Error::ParseError(format!("Invalid RSMF ZIP archive: {e}")))?;
        let mut manifest = None;
        for i in 0..archive.len() {
            let Ok(file) = archive.by_index(i) else {
                continue;
            };
            if file.is_dir() {
                continue;
            }
            let name = file.name().to_string();
            if name.ends_with("rsmf_manifest.json") {
                manifest = Some(read_bounded(file, 32 * 1024 * 1024)?);
            } else if media_bytes < MAX_RSMF_MEDIA_BYTES {
                // Keyed by full path: two folders can each hold a file with the same name.
                let buf = read_bounded(file, 32 * 1024 * 1024)?;
                media_bytes = media_bytes.saturating_add(buf.len());
                media_files.insert(name, buf);
            }
        }
        manifest.ok_or_else(|| {
            Error::ParseError("RSMF archive is missing rsmf_manifest.json".to_string())
        })?
    } else {
        data.to_vec()
    };

    let root: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| Error::ParseError(format!("Invalid RSMF manifest JSON: {e}")))?;

    let version = root
        .get("version")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let mut participant_lookup = BTreeMap::new();
    let mut participants = Vec::new();
    if let Some(arr) = root.get("participants").and_then(Value::as_array) {
        for item in arr {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let display = item
                .get("display")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let email = item
                .get("email")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            participant_lookup.insert(id.clone(), display.clone());
            participants.push(ChatParticipant {
                id,
                display_name: display,
                email,
            });
        }
    }

    let mut conversations = Vec::new();
    if let Some(arr) = root.get("conversations").and_then(Value::as_array) {
        for item in arr {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("conv_1")
                .to_string();
            let display_name = item
                .get("display")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let platform = item
                .get("platform")
                .and_then(Value::as_str)
                .unwrap_or("rsmf")
                .to_string();
            let conv_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("channel")
                .to_string();
            let conv_participants = item
                .get("participants")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default();
            conversations.push(ChatConversation {
                id,
                display_name,
                platform,
                conv_type,
                participants: conv_participants,
            });
        }
    }

    let mut events = Vec::new();
    if let Some(arr) = root.get("events").and_then(Value::as_array) {
        for (idx, item) in arr.iter().enumerate() {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("ev_{}", idx + 1), ToString::to_string);
            let conversation_id = item
                .get("conversation")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let participant_id = item
                .get("participant")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let participant_display = participant_id
                .as_deref()
                .and_then(|pid| participant_lookup.get(pid).cloned())
                .or_else(|| participant_id.clone())
                .unwrap_or_else(|| "System".to_string());
            let timestamp = item
                .get("timestamp")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let body = item
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let event_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("message")
                .to_string();
            let parent_id = item
                .get("parent")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let deleted = item
                .get("deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let mut reactions = Vec::new();
            if let Some(r_arr) = item.get("reactions").and_then(Value::as_array) {
                for r in r_arr {
                    let value = r
                        .get("value")
                        .or_else(|| r.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("👍")
                        .to_string();
                    let r_parts: Vec<String> = r
                        .get("participants")
                        .and_then(Value::as_array)
                        .map(|list| {
                            list.iter()
                                .filter_map(Value::as_str)
                                .map(ToString::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    let count = r
                        .get("count")
                        .and_then(Value::as_u64)
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or_else(|| r_parts.len().max(1));
                    reactions.push(ChatReaction {
                        value,
                        count,
                        participants: r_parts,
                    });
                }
            }

            let mut edits = Vec::new();
            if let Some(e_arr) = item.get("edits").and_then(Value::as_array) {
                for e in e_arr {
                    edits.push(ChatEdit {
                        participant: e
                            .get("participant")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        timestamp: e
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        previous: e
                            .get("previous")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                        new_text: e
                            .get("new")
                            .and_then(Value::as_str)
                            .map(ToString::to_string),
                    });
                }
            }

            let mut attachments = Vec::new();
            if let Some(a_arr) = item.get("attachments").and_then(Value::as_array) {
                for a in a_arr {
                    let att_id = a
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("attachment")
                        .to_string();
                    let display_name = a
                        .get("display")
                        .and_then(Value::as_str)
                        .unwrap_or(&att_id)
                        .to_string();
                    let size = a.get("size").and_then(Value::as_u64);
                    attachments.push(ChatAttachment {
                        id: att_id,
                        display_name,
                        mime_type: None,
                        size,
                    });
                }
            }

            events.push(ChatEvent {
                id,
                conversation_id,
                participant_id,
                participant_display,
                timestamp,
                body,
                event_type,
                parent_id,
                reactions,
                edits,
                deleted,
                attachments,
            });
        }
    }

    Ok(ChatTranscript {
        platform: ChatPlatform::Rsmf,
        version,
        participants,
        conversations,
        events,
        media_files,
    })
}

// ============================================================================
// Slack JSON Export Parser
// ============================================================================

fn parse_slack_json(data: &[u8]) -> Result<ChatTranscript> {
    let root: Value = serde_json::from_slice(data)
        .map_err(|e| Error::ParseError(format!("Invalid Slack JSON export: {e}")))?;

    let (messages_arr, channel_name) = if let Some(arr) = root.as_array() {
        (arr.as_slice(), "general".to_string())
    } else if let Some(arr) = root.get("messages").and_then(Value::as_array) {
        let ch = root
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("general")
            .to_string();
        (arr.as_slice(), ch)
    } else {
        return Err(Error::ParseError(
            "Slack JSON does not contain a messages array".to_string(),
        ));
    };

    let mut participant_map: BTreeMap<String, ChatParticipant> = BTreeMap::new();
    let mut events = Vec::new();

    for (idx, item) in messages_arr.iter().enumerate() {
        let subtype = item.get("subtype").and_then(Value::as_str).unwrap_or("");
        let user_id = item
            .get("user")
            .or_else(|| item.get("bot_id"))
            .and_then(Value::as_str)
            .unwrap_or("system")
            .to_string();
        let display_name = item
            .get("user_profile")
            .and_then(|p| {
                p.get("real_name")
                    .or_else(|| p.get("display_name"))
                    .and_then(Value::as_str)
            })
            .or_else(|| item.get("username").and_then(Value::as_str))
            .unwrap_or(&user_id)
            .to_string();

        if user_id != "system" {
            participant_map
                .entry(user_id.clone())
                .or_insert_with(|| ChatParticipant {
                    id: user_id.clone(),
                    display_name: display_name.clone(),
                    email: None,
                });
        }

        let ts_raw = item
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let timestamp = parse_slack_ts(&ts_raw).or_else(|| {
            if ts_raw.is_empty() {
                None
            } else {
                Some(ts_raw.clone())
            }
        });
        let parent_id = item
            .get("thread_ts")
            .and_then(Value::as_str)
            .filter(|&t| t != ts_raw)
            .map(ToString::to_string);

        let deleted = subtype == "message_deleted"
            || subtype == "tombstone"
            || item
                .get("deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false);

        let mut body = item
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if body.is_empty()
            && let Some(prev_msg) = item.get("previous_message")
            && let Some(prev_text) = prev_msg.get("text").and_then(Value::as_str)
        {
            body = prev_text.to_string();
        }

        let mut edits = Vec::new();
        if item.get("edited").is_some() || subtype == "message_changed" {
            let previous = item
                .get("previous_message")
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let edit_ts = item
                .get("edited")
                .and_then(|e| e.get("ts"))
                .and_then(Value::as_str)
                .and_then(parse_slack_ts);
            edits.push(ChatEdit {
                participant: Some(display_name.clone()),
                timestamp: edit_ts,
                previous,
                new_text: Some(body.clone()),
            });
        }

        let mut reactions = Vec::new();
        if let Some(r_arr) = item.get("reactions").and_then(Value::as_array) {
            for r in r_arr {
                let name = r
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("reaction")
                    .to_string();
                let users: Vec<String> = r
                    .get("users")
                    .and_then(Value::as_array)
                    .map(|u| {
                        u.iter()
                            .filter_map(Value::as_str)
                            .map(ToString::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let count = r
                    .get("count")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or_else(|| users.len().max(1));
                reactions.push(ChatReaction {
                    value: format!(":{name}:"),
                    count,
                    participants: users,
                });
            }
        }

        let mut attachments = Vec::new();
        if let Some(f_arr) = item.get("files").and_then(Value::as_array) {
            for f in f_arr {
                let fid = f
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_string();
                let fname = f
                    .get("name")
                    .or_else(|| f.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or(&fid)
                    .to_string();
                let mime = f
                    .get("mimetype")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                let size = f.get("size").and_then(Value::as_u64);
                attachments.push(ChatAttachment {
                    id: fid,
                    display_name: fname,
                    mime_type: mime,
                    size,
                });
            }
        }

        events.push(ChatEvent {
            id: if ts_raw.is_empty() {
                format!("slack_{}", idx + 1)
            } else {
                ts_raw
            },
            conversation_id: Some(channel_name.clone()),
            participant_id: Some(user_id),
            participant_display: display_name,
            timestamp,
            body,
            event_type: "message".to_string(),
            parent_id,
            reactions,
            edits,
            deleted,
            attachments,
        });
    }

    let participants: Vec<ChatParticipant> = participant_map.into_values().collect();
    let p_ids = participants.iter().map(|p| p.id.clone()).collect();
    let conversations = vec![ChatConversation {
        id: channel_name.clone(),
        display_name: format!("#{channel_name}"),
        platform: "slack".to_string(),
        conv_type: "channel".to_string(),
        participants: p_ids,
    }];

    Ok(ChatTranscript {
        platform: ChatPlatform::Slack,
        version: None,
        participants,
        conversations,
        events,
        media_files: BTreeMap::new(),
    })
}

fn parse_slack_ts(ts: &str) -> Option<String> {
    let secs_str = ts.split('.').next()?;
    let secs: i64 = secs_str.parse().ok()?;
    let dt: DateTime<Utc> = Utc.timestamp_opt(secs, 0).single()?;
    Some(dt.to_rfc3339())
}

// ============================================================================
// Microsoft Teams Chat Export Parser
// ============================================================================

fn parse_teams_json(data: &[u8]) -> Result<ChatTranscript> {
    let root: Value = serde_json::from_slice(data)
        .map_err(|e| Error::ParseError(format!("Invalid Microsoft Teams JSON export: {e}")))?;

    let messages_arr = if let Some(arr) = root.as_array() {
        arr.as_slice()
    } else if let Some(arr) = root.get("value").and_then(Value::as_array) {
        arr.as_slice()
    } else if let Some(arr) = root.get("messages").and_then(Value::as_array) {
        arr.as_slice()
    } else {
        return Err(Error::ParseError(
            "Microsoft Teams JSON does not contain a messages array".to_string(),
        ));
    };

    let mut participant_map: BTreeMap<String, ChatParticipant> = BTreeMap::new();
    let mut events = Vec::new();

    for (idx, item) in messages_arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("teams_{}", idx + 1), ToString::to_string);

        let user_obj = item.get("from").and_then(|f| f.get("user").or(Some(f)));
        let user_id = user_obj
            .and_then(|u| u.get("id").and_then(Value::as_str))
            .unwrap_or("teams_user")
            .to_string();
        let display_name = user_obj
            .and_then(|u| {
                u.get("displayName")
                    .or_else(|| u.get("display"))
                    .and_then(Value::as_str)
            })
            .unwrap_or(&user_id)
            .to_string();
        let email = user_obj
            .and_then(|u| u.get("email").and_then(Value::as_str))
            .map(ToString::to_string);

        participant_map
            .entry(user_id.clone())
            .or_insert_with(|| ChatParticipant {
                id: user_id.clone(),
                display_name: display_name.clone(),
                email,
            });

        let created = item
            .get("createdDateTime")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        // `lastModifiedDateTime` also changes when a reaction is added, so it only stands in
        // for `lastEditedDateTime` in exports that lack that field.
        let modified = match item.get("lastEditedDateTime") {
            Some(edited) => edited.as_str().map(ToString::to_string),
            None => item
                .get("lastModifiedDateTime")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        };
        let deleted = item.get("deletedDateTime").is_some_and(|v| !v.is_null())
            || item
                .get("deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false);

        let raw_body = item
            .get("body")
            .and_then(|b| b.get("content").or(Some(b)))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let is_html = item
            .get("body")
            .and_then(|b| b.get("contentType"))
            .and_then(Value::as_str)
            .is_some_and(|ct| ct.eq_ignore_ascii_case("html"));
        let body = if is_html {
            prism_core::text::html_to_text(raw_body)
        } else {
            raw_body.to_string()
        };

        let mut edits = Vec::new();
        if let Some(ref mod_ts) = modified
            && created.as_ref() != Some(mod_ts)
        {
            edits.push(ChatEdit {
                participant: Some(display_name.clone()),
                timestamp: Some(mod_ts.clone()),
                previous: item
                    .get("previousBody")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                new_text: Some(body.clone()),
            });
        }

        let mut reactions = Vec::new();
        if let Some(r_arr) = item.get("reactions").and_then(Value::as_array) {
            for r in r_arr {
                let r_type = r
                    .get("reactionType")
                    .or_else(|| r.get("value"))
                    .and_then(Value::as_str)
                    .unwrap_or("like")
                    .to_string();
                reactions.push(ChatReaction {
                    value: r_type,
                    count: 1,
                    participants: Vec::new(),
                });
            }
        }

        let mut attachments = Vec::new();
        if let Some(a_arr) = item.get("attachments").and_then(Value::as_array) {
            for a in a_arr {
                let aid = a
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment")
                    .to_string();
                let aname = a
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&aid)
                    .to_string();
                let mime = a
                    .get("contentType")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                attachments.push(ChatAttachment {
                    id: aid,
                    display_name: aname,
                    mime_type: mime,
                    size: None,
                });
            }
        }

        events.push(ChatEvent {
            id,
            conversation_id: Some("teams_chat".to_string()),
            participant_id: Some(user_id),
            participant_display: display_name,
            timestamp: created,
            body,
            event_type: "message".to_string(),
            parent_id: item
                .get("replyToId")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            reactions,
            edits,
            deleted,
            attachments,
        });
    }

    let participants: Vec<ChatParticipant> = participant_map.into_values().collect();
    let p_ids = participants.iter().map(|p| p.id.clone()).collect();
    let conversations = vec![ChatConversation {
        id: "teams_chat".to_string(),
        display_name: "Microsoft Teams Conversation".to_string(),
        platform: "teams".to_string(),
        conv_type: "channel".to_string(),
        participants: p_ids,
    }];

    Ok(ChatTranscript {
        platform: ChatPlatform::Teams,
        version: None,
        participants,
        conversations,
        events,
        media_files: BTreeMap::new(),
    })
}

// ============================================================================
// WhatsApp Text Transcript Parser
// ============================================================================

fn parse_whatsapp_text(data: &[u8]) -> Result<ChatTranscript> {
    let text = String::from_utf8_lossy(data);
    let mut participants_seen = BTreeSet::new();
    let mut events: Vec<ChatEvent> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_start_matches('\u{feff}').trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some((timestamp, sender, mut msg_body)) = split_whatsapp_line(line) {
            participants_seen.insert(sender.clone());
            let deleted = msg_body.contains("This message was deleted")
                || msg_body.contains("You deleted this message");
            let mut edits = Vec::new();
            if msg_body.contains("<This message was edited>") {
                msg_body = msg_body
                    .replace("<This message was edited>", "")
                    .trim()
                    .to_string();
                edits.push(ChatEdit {
                    participant: Some(sender.clone()),
                    timestamp: Some(timestamp.clone()),
                    previous: None,
                    new_text: Some(msg_body.clone()),
                });
            }

            let mut attachments = Vec::new();
            if let Some(start) = msg_body.find("<attached:")
                && let Some(end) = msg_body[start..].find('>')
            {
                let att_name = msg_body[start + 10..start + end].trim().to_string();
                attachments.push(ChatAttachment {
                    id: att_name.clone(),
                    display_name: att_name,
                    mime_type: None,
                    size: None,
                });
            }

            let id = format!("wa_{}", events.len() + 1);
            events.push(ChatEvent {
                id,
                conversation_id: Some("whatsapp_chat".to_string()),
                participant_id: Some(sender.clone()),
                participant_display: sender,
                timestamp: Some(timestamp),
                body: msg_body,
                event_type: "message".to_string(),
                parent_id: None,
                reactions: Vec::new(),
                edits,
                deleted,
                attachments,
            });
        } else if let Some(last) = events.last_mut() {
            last.body.push('\n');
            last.body.push_str(line);
        }
    }

    if events.is_empty() {
        return Err(Error::ParseError(
            "No valid WhatsApp chat lines found".to_string(),
        ));
    }

    let participants: Vec<ChatParticipant> = participants_seen
        .into_iter()
        .map(|name| ChatParticipant {
            id: name.clone(),
            display_name: name,
            email: None,
        })
        .collect();
    let p_ids = participants.iter().map(|p| p.id.clone()).collect();
    let conversations = vec![ChatConversation {
        id: "whatsapp_chat".to_string(),
        display_name: "WhatsApp Chat".to_string(),
        platform: "whatsapp".to_string(),
        conv_type: "direct".to_string(),
        participants: p_ids,
    }];

    Ok(ChatTranscript {
        platform: ChatPlatform::WhatsApp,
        version: None,
        participants,
        conversations,
        events,
        media_files: BTreeMap::new(),
    })
}

fn split_whatsapp_line(line: &str) -> Option<(String, String, String)> {
    if let Some(rest) = line.strip_prefix('[')
        && let Some((ts, after_bracket)) = rest.split_once("] ")
        && let Some((sender, msg)) = after_bracket.split_once(": ")
    {
        return Some((
            ts.trim().to_string(),
            sender.trim().to_string(),
            msg.to_string(),
        ));
    }
    if let Some((ts, after_dash)) = line.split_once(" - ")
        && ts.chars().next().is_some_and(|c| c.is_ascii_digit())
        && ts.contains(':')
        && let Some((sender, msg)) = after_dash.split_once(": ")
    {
        return Some((
            ts.trim().to_string(),
            sender.trim().to_string(),
            msg.to_string(),
        ));
    }
    None
}

// ============================================================================
// Parser trait implementations (RsmfParser, SlackParser, TeamsParser, WhatsAppParser)
// ============================================================================

/// Relativity Short Message Format (`.rsmf`) parser.
#[derive(Debug, Clone, Default)]
pub struct RsmfParser;

impl RsmfParser {
    /// Create a new [`RsmfParser`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for RsmfParser {
    fn format(&self) -> Format {
        Format::rsmf()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        prism_core::format::detect_format(data, Some("sample.rsmf"))
            .is_some_and(|r| r.format.extension == "rsmf")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let transcript = ChatTranscript::parse(&data, ChatPlatform::Rsmf)?;
        Ok(transcript.to_document(context.filename.as_deref()))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "Relativity Short Message Format (RSMF) Parser".to_string(),
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

/// Slack JSON chat export parser.
#[derive(Debug, Clone, Default)]
pub struct SlackParser;

impl SlackParser {
    /// Create a new [`SlackParser`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for SlackParser {
    fn format(&self) -> Format {
        Format::slack_chat()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        prism_core::format::detect_format(data, None).is_some_and(|r| r.format.extension == "slack")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let transcript = ChatTranscript::parse(&data, ChatPlatform::Slack)?;
        Ok(transcript.to_document(context.filename.as_deref()))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "Slack Chat Export Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// Microsoft Teams JSON chat export parser.
#[derive(Debug, Clone, Default)]
pub struct TeamsParser;

impl TeamsParser {
    /// Create a new [`TeamsParser`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for TeamsParser {
    fn format(&self) -> Format {
        Format::teams_chat()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        prism_core::format::detect_format(data, None).is_some_and(|r| r.format.extension == "teams")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let transcript = ChatTranscript::parse(&data, ChatPlatform::Teams)?;
        Ok(transcript.to_document(context.filename.as_deref()))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "Microsoft Teams Chat Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}

/// `WhatsApp` chat transcript parser.
#[derive(Debug, Clone, Default)]
pub struct WhatsAppParser;

impl WhatsAppParser {
    /// Create a new [`WhatsAppParser`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Parser for WhatsAppParser {
    fn format(&self) -> Format {
        Format::whatsapp_chat()
    }

    fn can_parse(&self, data: &[u8]) -> bool {
        prism_core::format::detect_format(data, None)
            .is_some_and(|r| r.format.extension == "whatsapp")
    }

    async fn parse(&self, data: Bytes, context: ParseContext) -> Result<Document> {
        let transcript = ChatTranscript::parse(&data, ChatPlatform::WhatsApp)?;
        Ok(transcript.to_document(context.filename.as_deref()))
    }

    fn metadata(&self) -> ParserMetadata {
        ParserMetadata {
            name: "WhatsApp Chat Transcript Parser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                ParserFeature::TextExtraction,
                ParserFeature::MetadataExtraction,
                ParserFeature::TableExtraction,
            ],
            requires_sandbox: false,
        }
    }
}
