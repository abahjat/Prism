// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 7 Integration Tests:
//! - M7.1: Native Outlook `.pst` & `.ost` Container Reader (`PstArchive`, `PstParser`, `build_family_tree`, `open_child_bytes`)
//! - M7.2: eDiscovery Chat & Short Message Formats (`RSMF`, Slack, Microsoft Teams, WhatsApp)

use prism_core::ENGINE_VERSION;
use prism_core::format::detect_format;
use prism_core::review::FindingKind;
use prism_core::tree::ContainerLimits;
use prism_layout::{LayoutDocument, LayoutOptions};
use prism_parsers::email::pst::{
    NDB_CRYPT_NONE, NDB_CRYPT_PERMUTE, PstFolderSpec, PstMessageSpec, build_pst_bytes,
};
use prism_parsers::pipeline::open_and_layout;
use prism_parsers::review::generate_hidden_content_report;
use prism_parsers::tree::{build_family_tree, open_child_bytes};
use prism_review::{
    Annotation, AnnotationAnchor, AnnotationKind, AnnotationSidecar, RedactedExportFormat,
    RedactionTarget, TextAnchor, apply_redactions, export_redacted_document,
};
use std::io::{Cursor, Write};

fn doc_text(doc: &LayoutDocument) -> String {
    doc.pages
        .iter()
        .map(prism_layout::LayoutPage::text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn m7_1_pst_and_ost_native_parsing_family_tree_and_redaction() {
    let folders = vec![
        PstFolderSpec {
            nid: 0x0122,
            parent_nid: 0x0122,
            display_name: "Top of Personal Folders".to_string(),
        },
        PstFolderSpec {
            nid: 0x0142,
            parent_nid: 0x0122,
            display_name: "Inbox".to_string(),
        },
        PstFolderSpec {
            nid: 0x0162,
            parent_nid: 0x0122,
            display_name: "Sent Items".to_string(),
        },
    ];
    let messages = vec![
        PstMessageSpec {
            nid: 0x20004,
            folder_nid: 0x0142,
            subject: "Q4 Acquisition Due Diligence".to_string(),
            sender_name: "Alice Vance".to_string(),
            sender_email: "alice.vance@acme.example".to_string(),
            display_to: "Bob Sterling <bob@acme.example>".to_string(),
            display_cc: Some("legal@acme.example".to_string()),
            timestamp: None,
            body_text: "Please review the attached valuation model. Project Codeword: HYPERION-99."
                .to_string(),
            attachments: vec![(
                "valuation_notes.txt".to_string(),
                "text/plain".to_string(),
                b"Confidential Valuation Multiplier: 14.5x EBITDA".to_vec(),
            )],
        },
        PstMessageSpec {
            nid: 0x20024,
            folder_nid: 0x0162,
            subject: "Re: Q4 Acquisition Due Diligence".to_string(),
            sender_name: "Bob Sterling".to_string(),
            sender_email: "bob@acme.example".to_string(),
            display_to: "Alice Vance <alice.vance@acme.example>".to_string(),
            display_cc: None,
            timestamp: None,
            body_text: "Confirmed. Legal approved the draft schedule.".to_string(),
            attachments: Vec::new(),
        },
    ];

    // 1. Build a permutation-encoded PST and an unencoded OST
    let pst_bytes = build_pst_bytes(&folders, &messages, NDB_CRYPT_PERMUTE, false);
    let ost_bytes = build_pst_bytes(&folders, &messages, NDB_CRYPT_NONE, true);

    let det_pst = detect_format(&pst_bytes, None).expect("detect PST magic");
    assert_eq!(det_pst.format.extension, "pst");
    let det_ost = detect_format(&ost_bytes, None).expect("detect OST magic");
    assert_eq!(det_ost.format.extension, "ost");

    // 2. Open and layout both PST and OST into paginated LayoutDocuments
    let opened_pst = open_and_layout(
        &pst_bytes,
        Some("custodian_vance.pst"),
        &LayoutOptions::default(),
    )
    .expect("open_and_layout PST");
    assert!(!opened_pst.layout.pages.is_empty());
    let pst_text = doc_text(&opened_pst.layout);
    assert!(pst_text.contains("Top of Personal Folders/Inbox"));
    assert!(pst_text.contains("Q4 Acquisition Due Diligence"));
    assert!(pst_text.contains("Alice Vance <alice.vance@acme.example>"));
    assert!(pst_text.contains("HYPERION-99"));
    assert!(pst_text.contains("valuation_notes.txt"));

    let opened_ost = open_and_layout(
        &ost_bytes,
        Some("cached_mailbox.ost"),
        &LayoutOptions::default(),
    )
    .expect("open_and_layout OST");
    assert!(doc_text(&opened_ost.layout).contains("HYPERION-99"));

    // 3. FamilyTree container walk extracts .eml messages and nested attachment grandchildren
    let tree = build_family_tree(
        &pst_bytes,
        Some("custodian_vance.pst"),
        ContainerLimits::default(),
    );
    assert!(
        tree.total_members >= 3,
        "Expected 2 synthesized .eml messages + 1 nested attachment, got {}",
        tree.total_members
    );
    let (att_node, att_payload) = open_child_bytes(
        &pst_bytes,
        Some("custodian_vance.pst"),
        "valuation_notes.txt",
        ContainerLimits::default(),
    )
    .expect("extract nested PST message attachment");
    assert_eq!(att_node.name, "valuation_notes.txt");
    assert_eq!(
        att_payload,
        b"Confidential Valuation Multiplier: 14.5x EBITDA"
    );

    // 4. HiddenContentReport surfaces PST author/message history and attachments
    let report = generate_hidden_content_report(
        &pst_bytes,
        Some("custodian_vance.pst"),
        Some(&opened_pst.layout),
    );
    assert!(report.has_kind(FindingKind::AuthorAndModificationHistory));
    assert!(report.has_kind(FindingKind::EmbeddedFile));

    // 5. Destructive redaction of sensitive codeword on PST layout page
    let (ta, rects) = TextAnchor::from_page_quote(&opened_pst.layout, 0, "HYPERION-99")
        .expect("anchor HYPERION-99");
    let mut sidecar = AnnotationSidecar::new(&pst_bytes);
    sidecar.add(Annotation {
        id: "redact_pst_1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "TRADE_SECRET".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::TextRange,
            page_index: 0,
            rects_pt: rects,
            text_anchor: Some(ta),
        },
        author: Some("reviewer".to_string()),
        created_at: Some("2026-10-24T12:00:00Z".to_string()),
        proposed: false,
    });
    let resolved = sidecar
        .resolve_against(&pst_bytes, ENGINE_VERSION, &opened_pst.layout)
        .expect("resolve PST sidecar");
    let mut redacted_doc = opened_pst.layout.clone();
    let log = apply_redactions(&mut redacted_doc, &sidecar.document_sha256, &resolved)
        .expect("apply PST redaction");
    assert_eq!(log.total_applied, 1);
    assert!(!doc_text(&redacted_doc).contains("HYPERION-99"));
    assert!(doc_text(&redacted_doc).contains("Alice Vance"));

    let exported_pdf = export_redacted_document(
        &pst_bytes,
        &opened_pst.layout,
        &opened_pst.metadata,
        &sidecar,
        RedactedExportFormat::Pdf,
        150.0,
    )
    .expect("verified redacted PDF export for PST");
    assert!(!exported_pdf.bytes.is_empty());
}

#[test]
fn m7_2_rsmf_slack_teams_and_whatsapp_chat_rendering_and_forensics() {
    // 1. Build an RSMF ZIP container with participants, conversations, reactions, edits, and deleted messages
    let rsmf_manifest = serde_json::json!({
        "version": "2.0.0",
        "participants": [
            { "id": "P1", "display": "Sarah Chen", "email": "sarah.chen@corp.example" },
            { "id": "P2", "display": "Marcus Wright", "email": "marcus.wright@corp.example" }
        ],
        "conversations": [
            {
                "id": "C1",
                "display": "#deal-room-alpha",
                "platform": "slack",
                "type": "channel",
                "participants": ["P1", "P2"]
            }
        ],
        "events": [
            {
                "id": "m1",
                "type": "message",
                "conversation": "C1",
                "participant": "P1",
                "timestamp": "2026-10-20T09:00:00Z",
                "body": "Did we finalize the target share price of $42.50?",
                "reactions": [
                    { "value": "👍", "count": 2, "participants": ["P1", "P2"] }
                ]
            },
            {
                "id": "m2",
                "type": "message",
                "conversation": "C1",
                "participant": "P2",
                "parent": "m1",
                "timestamp": "2026-10-20T09:02:15Z",
                "body": "Yes, updated to $44.00 per share after board review.",
                "edits": [
                    {
                        "participant": "P2",
                        "timestamp": "2026-10-20T09:05:00Z",
                        "previous": "Yes, locked at $41.00 per share.",
                        "new": "Yes, updated to $44.00 per share after board review."
                    }
                ],
                "attachments": [
                    { "id": "term_sheet.txt", "display": "term_sheet.txt", "size": 34 }
                ]
            },
            {
                "id": "m3",
                "type": "message",
                "conversation": "C1",
                "participant": "P1",
                "timestamp": "2026-10-20T09:06:00Z",
                "body": "Shred the draft memo before the auditors arrive.",
                "deleted": true
            }
        ]
    });

    let mut rsmf_zip_cursor = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut rsmf_zip_cursor);
        let opts = zip::write::FileOptions::default();
        zip.start_file("rsmf_manifest.json", opts).unwrap();
        zip.write_all(rsmf_manifest.to_string().as_bytes()).unwrap();
        zip.start_file("term_sheet.txt", opts).unwrap();
        zip.write_all(b"Binding Term Sheet: $44.00/share").unwrap();
        zip.finish().unwrap();
    }
    let rsmf_bytes = rsmf_zip_cursor.into_inner();

    // Auto-detect RSMF from ZIP container without filename hint
    let det_rsmf = detect_format(&rsmf_bytes, None).expect("detect RSMF");
    assert_eq!(det_rsmf.format.extension, "rsmf");

    // Layout RSMF into paginated bubble timeline
    let opened_rsmf = open_and_layout(
        &rsmf_bytes,
        Some("thread_001.rsmf"),
        &LayoutOptions::default(),
    )
    .expect("open_and_layout RSMF");
    let rsmf_text = doc_text(&opened_rsmf.layout);
    assert!(rsmf_text.contains("Sarah Chen"));
    assert!(rsmf_text.contains("Marcus Wright"));
    assert!(rsmf_text.contains("#deal-room-alpha"));
    assert!(rsmf_text.contains("[EDITED]"));
    assert!(rsmf_text.contains("[DELETED]"));
    assert!(rsmf_text.contains("Yes, locked at $41.00 per share."));
    assert!(rsmf_text.contains("Shred the draft memo before the auditors arrive."));

    // FamilyTree extracts rsmf_manifest.json and term_sheet.txt
    let rsmf_tree = build_family_tree(
        &rsmf_bytes,
        Some("thread_001.rsmf"),
        ContainerLimits::default(),
    );
    assert_eq!(rsmf_tree.total_members, 2);
    let (_, sheet_bytes) = open_child_bytes(
        &rsmf_bytes,
        Some("thread_001.rsmf"),
        "term_sheet.txt",
        ContainerLimits::default(),
    )
    .expect("extract RSMF attachment");
    assert_eq!(sheet_bytes, b"Binding Term Sheet: $44.00/share");

    // HiddenContentReport surfaces TrackedDeletion (m3), TrackedInsertion (m2 edit), and EmbeddedFile
    let rsmf_report = generate_hidden_content_report(
        &rsmf_bytes,
        Some("thread_001.rsmf"),
        Some(&opened_rsmf.layout),
    );
    assert!(rsmf_report.has_kind(FindingKind::TrackedDeletion));
    assert!(rsmf_report.has_kind(FindingKind::TrackedInsertion));
    assert!(rsmf_report.has_kind(FindingKind::EmbeddedFile));

    // Message-level redaction on RSMF bubble timeline
    let (ta, rects) = TextAnchor::from_page_quote(&opened_rsmf.layout, 0, "$42.50")
        .expect("anchor $42.50 in RSMF");
    let mut sidecar = AnnotationSidecar::new(&rsmf_bytes);
    sidecar.add(Annotation {
        id: "redact_rsmf_1".to_string(),
        kind: AnnotationKind::Redaction {
            reason_code: "CONFIDENTIAL".to_string(),
            label: "REDACTED".to_string(),
            fill_color: None,
        },
        anchor: AnnotationAnchor {
            target: RedactionTarget::TextRange,
            page_index: 0,
            rects_pt: rects,
            text_anchor: Some(ta),
        },
        author: Some("reviewer".to_string()),
        created_at: Some("2026-10-24T12:00:00Z".to_string()),
        proposed: false,
    });
    let resolved = sidecar
        .resolve_against(&rsmf_bytes, ENGINE_VERSION, &opened_rsmf.layout)
        .expect("resolve RSMF sidecar");
    let mut redacted_rsmf = opened_rsmf.layout.clone();
    let rsmf_log = apply_redactions(&mut redacted_rsmf, &sidecar.document_sha256, &resolved)
        .expect("apply RSMF redaction");
    assert_eq!(rsmf_log.total_applied, 1);
    assert!(!doc_text(&redacted_rsmf).contains("$42.50"));

    let exported_rsmf = export_redacted_document(
        &rsmf_bytes,
        &opened_rsmf.layout,
        &opened_rsmf.metadata,
        &sidecar,
        RedactedExportFormat::Pdf,
        150.0,
    )
    .expect("verified redacted PDF export for RSMF");
    assert!(!exported_rsmf.bytes.is_empty());

    // 2. Slack JSON export detection, layout, and forensic report
    let slack_json = br#"[
        {
            "type": "message",
            "user": "U01ALICE",
            "user_profile": { "real_name": "Alice Vance" },
            "text": "Deploying hotfix to production cluster.",
            "ts": "1700000100.000100",
            "reactions": [{ "name": "rocket", "count": 3, "users": ["U01ALICE"] }]
        },
        {
            "type": "message",
            "subtype": "message_deleted",
            "user": "U02BOB",
            "username": "Bob Sterling",
            "text": "Oops, leaked staging API token: sk-live-98765",
            "ts": "1700000200.000200"
        }
    ]"#;
    let det_slack = detect_format(slack_json, None).expect("detect Slack JSON");
    assert_eq!(det_slack.format.extension, "slack");
    let opened_slack = open_and_layout(slack_json, None, &LayoutOptions::default())
        .expect("open_and_layout Slack");
    assert!(doc_text(&opened_slack.layout).contains("Alice Vance"));
    assert!(doc_text(&opened_slack.layout).contains("[DELETED]"));
    let slack_report = generate_hidden_content_report(slack_json, None, Some(&opened_slack.layout));
    assert!(slack_report.has_kind(FindingKind::TrackedDeletion));

    // 3. Microsoft Teams JSON export detection, layout, and forensic report
    let teams_json = br#"{
        "value": [
            {
                "id": "169999001",
                "messageType": "message",
                "createdDateTime": "2026-10-21T10:00:00Z",
                "lastModifiedDateTime": "2026-10-21T10:05:00Z",
                "from": { "user": { "id": "u1", "displayName": "Elena Rostova", "email": "elena@teams.example" } },
                "body": { "contentType": "html", "content": "<p>Updated compliance sign-off status: <b>APPROVED</b></p>" }
            }
        ]
    }"#;
    let det_teams = detect_format(teams_json, None).expect("detect Teams JSON");
    assert_eq!(det_teams.format.extension, "teams");
    let opened_teams = open_and_layout(teams_json, None, &LayoutOptions::default())
        .expect("open_and_layout Teams");
    let teams_text = doc_text(&opened_teams.layout);
    assert!(teams_text.contains("Elena Rostova"));
    assert!(teams_text.contains("APPROVED"));
    assert!(teams_text.contains("[EDITED]"));
    let teams_report = generate_hidden_content_report(teams_json, None, Some(&opened_teams.layout));
    assert!(teams_report.has_kind(FindingKind::TrackedInsertion));

    // 4. WhatsApp chat transcript detection, layout, and forensic report
    let whatsapp_txt = b"[2026-10-22, 08:15:00] Carlos Mendez: Morning team, wire transfer sent.\n\
[2026-10-22, 08:16:30] Priya Patel: Received confirmation <This message was edited>\n\
[2026-10-22, 08:17:45] Carlos Mendez: This message was deleted\n";
    let det_wa = detect_format(whatsapp_txt, None).expect("detect WhatsApp text");
    assert_eq!(det_wa.format.extension, "whatsapp");
    let opened_wa = open_and_layout(whatsapp_txt, None, &LayoutOptions::default())
        .expect("open_and_layout WhatsApp");
    let wa_text = doc_text(&opened_wa.layout);
    assert!(wa_text.contains("Carlos Mendez"));
    assert!(wa_text.contains("Priya Patel"));
    assert!(wa_text.contains("[EDITED]"));
    assert!(wa_text.contains("[DELETED]"));
    let wa_report = generate_hidden_content_report(whatsapp_txt, None, Some(&opened_wa.layout));
    assert!(wa_report.has_kind(FindingKind::TrackedDeletion));
    assert!(wa_report.has_kind(FindingKind::TrackedInsertion));
}
