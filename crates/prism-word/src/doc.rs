// SPDX-License-Identifier: AGPL-3.0-only
//! Pure-Rust legacy binary Word (`[MS-DOC]`, Word 97–2003 and Word 2.0/6.0/95 fallback) reader.
//!
//! Reads the OLE2 / Compound File Binary (`cfb`) container, File Information Block (`FIB`),
//! Piece Table (`Clx` / `PlcPcd` supporting compressed 8-bit `CP-1252` and 16-bit `UTF-16LE`),
//! Character and Paragraph Formatted Disk Pages (`PlcfBteChpx`, `PlcfBtePapx`, and `grpprl` SPRMs),
//! inline field markers (`0x13`/`0x14`/`0x15`), table cells/rows (`0x07` + `sprmPFInTable`/`sprmPFTtp`),
//! and forensic indicators (`/Macros`, `/_VBA_PROJECT_CUR`, `fEncrypted`), producing a [`WordDocument`].

use crate::docx::classify_field_instruction;
use crate::model::{
    ExternalReference, FieldKind, ParagraphContent, ParagraphProperties, RevisionKind, RunItem,
    RunProperties, SectionProperties, TableProperties, ToggleProp, UnderlineKind, VerticalRunAlign,
    WordAlignment, WordBlock, WordDocument, WordField, WordHyperlink, WordLineSpacing,
    WordParagraph, WordRevisionMeta, WordRun, WordSection, WordStory, WordTable, WordTableCell,
    WordTableRow,
};
use cfb::CompoundFile;
use prism_core::error::{Error, Result};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

/// OLE2 Compound File Binary magic header (`D0 CF 11 E0 A1 B1 1A E1`).
pub const OLE2_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// Word 2.0 / 6.0 non-OLE raw magic header (`DB A5 2D 00`).
pub const WORD2_MAGIC: [u8; 4] = [0xDB, 0xA5, 0x2D, 0x00];

/// Default maximum stream size read from a `.doc` CFB container (`64 MiB`).
pub const DEFAULT_MAX_DOC_STREAM_BYTES: usize = 64 * 1024 * 1024;

/// Lowest `FibBase.nFib` with the Word 97 FIB layout. Word 6.0 (`0x65`) and Word 95 (`0x68`)
/// use a different layout and are left to the caller's text fallback.
const MIN_WORD97_NFIB: u16 = 0x00C0;

/// Maximum nesting of inline fields (`0x13` … `0x15`); deeper marks are kept as text.
const MAX_FIELD_DEPTH: usize = 32;

/// Quick check whether `data` is a `.doc` file (OLE2 with `/WordDocument` stream or Word 2.0 magic).
#[must_use]
pub fn is_doc_bytes(data: &[u8]) -> bool {
    if data.starts_with(&WORD2_MAGIC) {
        return true;
    }
    if !data.starts_with(&OLE2_MAGIC) {
        return false;
    }
    if let Ok(comp) = CompoundFile::open(Cursor::new(data)) {
        return comp.exists("WordDocument") || comp.exists("/WordDocument");
    }
    false
}

/// Parse a binary `.doc` file into a [`WordDocument`].
///
/// # Errors
///
/// Returns [`Error::EncryptedDocument`] if `FIB.fEncrypted` is set, or [`Error::ParseError`]
/// / [`Error::InvalidInput`] if the container is malformed or exceeds `max_stream_bytes`.
pub fn parse_doc(data: &[u8], max_stream_bytes: usize) -> Result<WordDocument> {
    if data.starts_with(&WORD2_MAGIC) {
        return Ok(parse_word2_raw(data));
    }
    if !data.starts_with(&OLE2_MAGIC) {
        return Err(Error::ParseError(
            "Invalid .doc header: expected OLE2 CFB or Word 2.0 magic".to_string(),
        ));
    }

    let mut comp = CompoundFile::open(Cursor::new(data))
        .map_err(|e| Error::ParseError(format!("Failed to open OLE2 .doc container: {e}")))?;

    let mut doc = WordDocument::default();

    // Check for VBA macro storages/streams
    if comp.exists("Macros")
        || comp.exists("/Macros")
        || comp.exists("_VBA_PROJECT_CUR")
        || comp.exists("/_VBA_PROJECT_CUR")
        || comp.exists("VBA")
        || comp.exists("/VBA")
    {
        doc.forensic.vba_project_present = true;
    }

    let word_stream = read_cfb_stream(&mut comp, "WordDocument", max_stream_bytes)?;
    if word_stream.len() < 32 {
        return Err(Error::ParseError(
            "WordDocument stream is shorter than FIB base (32 bytes)".to_string(),
        ));
    }

    let n_fib = read_u16(&word_stream, 2);
    if n_fib < MIN_WORD97_NFIB {
        return Err(Error::UnsupportedFormat(format!(
            "Word 6/95 binary format (nFib {n_fib:#06x}) is not supported by the [MS-DOC] reader"
        )));
    }

    let flags1 = read_u16(&word_stream, 10);
    let is_encrypted = (flags1 & 0x0100) != 0;
    if is_encrypted {
        return Err(Error::Encrypted(
            "Legacy .doc file is encrypted (FIB.fEncrypted = 1)".to_string(),
        ));
    }
    let use_1table = (flags1 & 0x0200) != 0;
    let table_name = if use_1table { "1Table" } else { "0Table" };
    let alt_table_name = if use_1table { "0Table" } else { "1Table" };

    let table_stream = read_cfb_stream(&mut comp, table_name, max_stream_bytes)
        .or_else(|_| read_cfb_stream(&mut comp, alt_table_name, max_stream_bytes))
        .unwrap_or_default();

    let fib = parse_fib(&word_stream);
    if fib.ccp_mcr > 0 {
        doc.forensic.vba_project_present = true;
    }

    // Decode pieces (either from Clx in table stream or from FIB fcMin..fcMac)
    let pieces = decode_pieces(&word_stream, &table_stream, &fib);

    // Decode CHPX and PAPX formatting runs mapped to file offsets (fc)
    let mut chpx_runs = decode_chpx_bte(&word_stream, &table_stream, &fib);
    let mut papx_runs = decode_papx_bte(&word_stream, &table_stream, &fib);
    // Sorted so `find_interval` can binary-search them.
    chpx_runs.sort_by_key(|r| r.fc_start);
    papx_runs.sort_by_key(|r| r.fc_start);

    // Reconstruct full character buffer with per-character FC offsets
    let (chars, char_fcs) = reconstruct_text_and_fcs(&word_stream, &pieces);

    let main_cp_end = (fib.ccp_text as usize).min(chars.len());
    let main_chars = if main_cp_end > 0 {
        &chars[..main_cp_end]
    } else {
        &chars[..]
    };
    let main_fcs = if main_cp_end > 0 {
        &char_fcs[..main_cp_end]
    } else {
        &char_fcs[..]
    };

    let blocks = build_blocks_from_cp_slice(main_chars, main_fcs, &chpx_runs, &papx_runs, &mut doc);

    doc.sections.push(WordSection {
        props: SectionProperties::default(),
        blocks,
    });

    if main_cp_end > 0 {
        push_unplaced_stories(
            &mut doc,
            &fib,
            &chars,
            &char_fcs,
            main_cp_end,
            &chpx_runs,
            &papx_runs,
        );
    }

    Ok(doc)
}

/// Keep the stories that follow the main text, in this CP order ([MS-DOC] 2.4.1). Where their
/// text belongs (note references, header kinds, comment anchors) is not decoded yet, so it is
/// kept as unplaced stories rather than dropped.
fn push_unplaced_stories(
    doc: &mut WordDocument,
    fib: &FibInfo,
    chars: &[char],
    char_fcs: &[u32],
    main_cp_end: usize,
    chpx_runs: &[ChpxInterval],
    papx_runs: &[PapxInterval],
) {
    let stories = [
        ("footnotes", fib.ccp_ftn),
        ("headers", fib.ccp_hdd),
        ("macro", fib.ccp_mcr),
        ("annotations", fib.ccp_atn),
        ("endnotes", fib.ccp_edn),
        ("textboxes", fib.ccp_txbx),
        ("header-textboxes", fib.ccp_hdr_txbx),
    ];
    let mut cp = main_cp_end;
    for (id, len) in stories {
        let end = cp.saturating_add(len as usize).min(chars.len());
        if end > cp {
            let blocks = build_blocks_from_cp_slice(
                &chars[cp..end],
                &char_fcs[cp..end],
                chpx_runs,
                papx_runs,
                doc,
            );
            if !blocks.is_empty() {
                doc.unplaced_stories.push(WordStory {
                    id: id.to_string(),
                    blocks,
                });
            }
        }
        cp = end;
    }
}

#[derive(Debug, Default)]
struct FibInfo {
    fc_min: u32,
    fc_mac: u32,
    ccp_text: u32,
    ccp_ftn: u32,
    ccp_hdd: u32,
    ccp_mcr: u32,
    ccp_atn: u32,
    ccp_edn: u32,
    ccp_txbx: u32,
    ccp_hdr_txbx: u32,
    fc_clx: u32,
    lcb_clx: u32,
    fc_plcfbte_chpx: u32,
    lcb_plcfbte_chpx: u32,
    fc_plcfbte_papx: u32,
    lcb_plcfbte_papx: u32,
}

fn parse_fib(ws: &[u8]) -> FibInfo {
    let mut fib = FibInfo {
        fc_min: read_u32(ws, 24),
        fc_mac: read_u32(ws, 28),
        ..FibInfo::default()
    };

    if ws.len() < 64 {
        return fib;
    }

    let csw = usize::from(read_u16(ws, 32));
    let rg_lw_offset = 34 + csw * 2;
    if rg_lw_offset + 2 > ws.len() {
        return fib;
    }
    let clw = usize::from(read_u16(ws, rg_lw_offset));
    let lw_start = rg_lw_offset + 2;

    if lw_start + 44 <= ws.len() {
        fib.ccp_text = read_u32(ws, lw_start + 12);
        fib.ccp_ftn = read_u32(ws, lw_start + 16);
        fib.ccp_hdd = read_u32(ws, lw_start + 20);
        fib.ccp_mcr = read_u32(ws, lw_start + 24);
        fib.ccp_atn = read_u32(ws, lw_start + 28);
        fib.ccp_edn = read_u32(ws, lw_start + 32);
        fib.ccp_txbx = read_u32(ws, lw_start + 36);
        fib.ccp_hdr_txbx = read_u32(ws, lw_start + 40);
    }

    let rg_fclcb_offset = lw_start + clw * 4;
    if rg_fclcb_offset + 2 > ws.len() {
        return fib;
    }
    let cb_rg_fc_lcb = usize::from(read_u16(ws, rg_fclcb_offset));
    let fclcb_start = rg_fclcb_offset + 2;

    let read_pair = |idx: usize| -> (u32, u32) {
        let off = fclcb_start + idx * 8;
        if idx < cb_rg_fc_lcb && off + 8 <= ws.len() {
            (read_u32(ws, off), read_u32(ws, off + 4))
        } else {
            (0, 0)
        }
    };

    (fib.fc_plcfbte_chpx, fib.lcb_plcfbte_chpx) = read_pair(12);
    (fib.fc_plcfbte_papx, fib.lcb_plcfbte_papx) = read_pair(13);
    (fib.fc_clx, fib.lcb_clx) = read_pair(33);

    fib
}

#[derive(Debug, Clone)]
struct PieceDescriptor {
    cp_start: u32,
    cp_end: u32,
    byte_offset: usize,
    is_compressed_cp1252: bool,
}

fn decode_pieces(ws: &[u8], table: &[u8], fib: &FibInfo) -> Vec<PieceDescriptor> {
    let fc_clx = fib.fc_clx as usize;
    let lcb_clx = fib.lcb_clx as usize;

    if lcb_clx >= 5 && fc_clx + lcb_clx <= table.len() {
        let clx = &table[fc_clx..fc_clx + lcb_clx];
        let mut pos = 0usize;
        while pos < clx.len() {
            let clxt = clx[pos];
            pos += 1;
            if clxt == 1 {
                // RgPrc: 2-byte size + grpprl
                if pos + 2 > clx.len() {
                    break;
                }
                let cb = read_u16(clx, pos) as usize;
                pos = pos.saturating_add(2).saturating_add(cb);
            } else if clxt == 2 {
                // Pcdt: 4-byte lcb + PlcPcd
                if pos + 4 > clx.len() {
                    break;
                }
                let lcb = read_u32(clx, pos) as usize;
                pos += 4;
                if pos + lcb <= clx.len() && lcb >= 16 {
                    let plc = &clx[pos..pos + lcb];
                    let n = (lcb - 4) / 12;
                    let mut pieces = Vec::with_capacity(n);
                    let cp_bytes = (n + 1) * 4;
                    if cp_bytes + n * 8 <= plc.len() {
                        for i in 0..n {
                            let cp_start = read_u32(plc, i * 4);
                            let cp_end = read_u32(plc, (i + 1) * 4);
                            if cp_end <= cp_start {
                                continue;
                            }
                            let pcd_off = cp_bytes + i * 8;
                            let fc_raw = read_u32(plc, pcd_off + 2);
                            let is_compressed = (fc_raw & 0x4000_0000) != 0;
                            let byte_offset = if is_compressed {
                                ((fc_raw & 0x3FFF_FFFF) / 2) as usize
                            } else {
                                (fc_raw & 0x3FFF_FFFF) as usize
                            };
                            pieces.push(PieceDescriptor {
                                cp_start,
                                cp_end,
                                byte_offset,
                                is_compressed_cp1252: is_compressed,
                            });
                        }
                        if !pieces.is_empty() {
                            return pieces;
                        }
                    }
                }
                break;
            } else {
                break;
            }
        }
    }

    // Fallback: use FIB fcMin..fcMac (or offset 512..ws.len()) as a single CP-1252 piece
    let start = (fib.fc_min as usize).clamp(0, ws.len());
    let end = (fib.fc_mac as usize).clamp(start, ws.len());
    let (byte_offset, len) = if end > start {
        (start, end - start)
    } else if ws.len() > 512 {
        (512, ws.len() - 512)
    } else {
        (0, ws.len())
    };
    #[allow(clippy::cast_possible_truncation)]
    let cp_end = len as u32;
    vec![PieceDescriptor {
        cp_start: 0,
        cp_end,
        byte_offset,
        is_compressed_cp1252: true,
    }]
}

fn reconstruct_text_and_fcs(ws: &[u8], pieces: &[PieceDescriptor]) -> (Vec<char>, Vec<u32>) {
    let mut chars = Vec::new();
    let mut fcs = Vec::new();
    // Overlapping pieces could otherwise repeat the stream without bound; a valid piece
    // table never yields more characters than the stream has bytes.
    let max_chars = ws.len();

    for piece in pieces {
        let count = (piece.cp_end.saturating_sub(piece.cp_start) as usize).min(max_chars);
        for i in 0..count {
            if chars.len() >= max_chars {
                break;
            }
            if piece.is_compressed_cp1252 {
                let off = piece.byte_offset.saturating_add(i);
                if off >= ws.len() {
                    break;
                }
                let ch = decode_cp1252_byte(ws[off]);
                chars.push(ch);
                #[allow(clippy::cast_possible_truncation)]
                fcs.push(off as u32);
            } else {
                let off = piece.byte_offset.saturating_add(i * 2);
                if off + 2 > ws.len() {
                    break;
                }
                let unit = read_u16(ws, off);
                let ch = char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}');
                chars.push(ch);
                #[allow(clippy::cast_possible_truncation)]
                fcs.push(off as u32);
            }
        }
    }

    (chars, fcs)
}

#[derive(Debug, Clone)]
struct ChpxInterval {
    fc_start: u32,
    fc_end: u32,
    props: RunProperties,
    is_deleted: bool,
    is_inserted: bool,
}

#[derive(Debug, Clone)]
struct PapxInterval {
    fc_start: u32,
    fc_end: u32,
    props: ParagraphProperties,
    in_table: bool,
    is_row_end: bool,
}

fn decode_chpx_bte(ws: &[u8], table: &[u8], fib: &FibInfo) -> Vec<ChpxInterval> {
    let fc = fib.fc_plcfbte_chpx as usize;
    let lcb = fib.lcb_plcfbte_chpx as usize;
    if lcb < 12 || fc + lcb > table.len() {
        return Vec::new();
    }
    let plc = &table[fc..fc + lcb];
    let n = (lcb - 4) / 8;
    let fc_bytes = (n + 1) * 4;
    if fc_bytes + n * 4 > plc.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..n.min(4096) {
        let pn = (read_u32(plc, fc_bytes + i * 4) & 0x003F_FFFF) as usize;
        let page_off = pn.saturating_mul(512);
        if page_off + 512 > ws.len() {
            continue;
        }
        let fkp = &ws[page_off..page_off + 512];
        let crun = usize::from(fkp[511]);
        let rgfc_bytes = (crun + 1) * 4;
        if crun == 0 || rgfc_bytes + crun > 511 {
            continue;
        }
        for r in 0..crun {
            let fc_start = read_u32(fkp, r * 4);
            let fc_end = read_u32(fkp, (r + 1) * 4);
            let b_off = usize::from(fkp[rgfc_bytes + r]) * 2;
            if b_off == 0 || b_off >= 511 {
                continue;
            }
            let cb = usize::from(fkp[b_off]);
            if b_off + 1 + cb > 511 {
                continue;
            }
            let grpprl = &fkp[b_off + 1..b_off + 1 + cb];
            let (props, is_deleted, is_inserted) = parse_chpx_grpprl(grpprl);
            out.push(ChpxInterval {
                fc_start,
                fc_end,
                props,
                is_deleted,
                is_inserted,
            });
        }
    }
    out
}

fn decode_papx_bte(ws: &[u8], table: &[u8], fib: &FibInfo) -> Vec<PapxInterval> {
    let fc = fib.fc_plcfbte_papx as usize;
    let lcb = fib.lcb_plcfbte_papx as usize;
    if lcb < 12 || fc + lcb > table.len() {
        return Vec::new();
    }
    let plc = &table[fc..fc + lcb];
    let n = (lcb - 4) / 8;
    let fc_bytes = (n + 1) * 4;
    if fc_bytes + n * 4 > plc.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..n.min(4096) {
        let pn = (read_u32(plc, fc_bytes + i * 4) & 0x003F_FFFF) as usize;
        let page_off = pn.saturating_mul(512);
        if page_off + 512 > ws.len() {
            continue;
        }
        let fkp = &ws[page_off..page_off + 512];
        let crun = usize::from(fkp[511]);
        let rgfc_bytes = (crun + 1) * 4;
        if crun == 0 || rgfc_bytes + crun * 13 > 511 {
            continue;
        }
        for r in 0..crun {
            let fc_start = read_u32(fkp, r * 4);
            let fc_end = read_u32(fkp, (r + 1) * 4);
            let bx_off = rgfc_bytes + r * 13;
            let prop_offset = usize::from(fkp[bx_off]) * 2;
            if prop_offset == 0 || prop_offset >= 511 {
                continue;
            }
            let cw = usize::from(fkp[prop_offset]);
            let (grpprl_start, grpprl_len) = if cw == 0 {
                if prop_offset + 1 >= 511 {
                    continue;
                }
                let cb2 = usize::from(fkp[prop_offset + 1]) * 2;
                (prop_offset + 4, cb2.saturating_sub(2))
            } else {
                (prop_offset + 3, (cw * 2).saturating_sub(2))
            };
            if grpprl_start + grpprl_len > 511 {
                continue;
            }
            let grpprl = &fkp[grpprl_start..grpprl_start + grpprl_len];
            let (props, in_table, is_row_end) = parse_papx_grpprl(grpprl);
            out.push(PapxInterval {
                fc_start,
                fc_end,
                props,
                in_table,
                is_row_end,
            });
        }
    }
    out
}

/// Parse a `grpprl` byte array of Character SPRMs (`[MS-DOC]` §2.6.1).
#[allow(clippy::similar_names)]
fn parse_chpx_grpprl(grpprl: &[u8]) -> (RunProperties, bool, bool) {
    let mut rpr = RunProperties::default();
    let mut is_deleted = false;
    let mut is_inserted = false;
    let mut pos = 0usize;

    while pos + 2 <= grpprl.len() {
        let sprm = read_u16(grpprl, pos);
        pos += 2;
        let spra = (sprm >> 13) & 0x07;
        let op_len = match spra {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            6 => {
                if pos >= grpprl.len() {
                    break;
                }
                let l = usize::from(grpprl[pos]);
                pos += 1;
                l
            }
            _ => break,
        };
        if pos + op_len > grpprl.len() {
            break;
        }
        let op = &grpprl[pos..pos + op_len];
        pos += op_len;

        match sprm {
            0x0800 => is_deleted = op[0] != 0,
            0x0801 => is_inserted = op[0] != 0,
            0x0835 => rpr.bold = Some(toggle_from_sprm_byte(op[0])),
            0x0836 => rpr.italic = Some(toggle_from_sprm_byte(op[0])),
            0x0837 => rpr.strike = Some(toggle_from_sprm_byte(op[0])),
            0x2A53 => rpr.double_strike = Some(toggle_from_sprm_byte(op[0])),
            0x083B => rpr.small_caps = Some(toggle_from_sprm_byte(op[0])),
            0x083C => rpr.caps = Some(toggle_from_sprm_byte(op[0])),
            0x083D => rpr.vanish = Some(toggle_from_sprm_byte(op[0])),
            0x2A3E => {
                rpr.underline = Some(match op[0] {
                    0 => UnderlineKind::None,
                    2 => UnderlineKind::Words,
                    3 => UnderlineKind::Double,
                    4 => UnderlineKind::Dotted,
                    7 => UnderlineKind::Dashed,
                    11 => UnderlineKind::Wave,
                    _ => UnderlineKind::Single,
                });
            }
            0x4A43 if op.len() >= 2 => {
                let half_pts = read_u16(op, 0);
                if half_pts > 0 {
                    rpr.font_size_pt = Some(f32::from(half_pts) * 0.5);
                }
            }
            0x2A48 => {
                rpr.vert_align = Some(match op[0] {
                    1 => VerticalRunAlign::Superscript,
                    2 => VerticalRunAlign::Subscript,
                    _ => VerticalRunAlign::Baseline,
                });
            }
            0x2A42 => {
                if let Some(hex) = ico_color_hex(op[0]) {
                    rpr.color = Some(format!("#{hex}"));
                }
            }
            0x6870 if op.len() >= 4 && op[3] != 0xFF => {
                rpr.color = Some(format!("#{:02X}{:02X}{:02X}", op[0], op[1], op[2]));
            }
            _ => {}
        }
    }

    (rpr, is_deleted, is_inserted)
}

/// Parse a `grpprl` byte array of Paragraph SPRMs (`[MS-DOC]` §2.6.2).
#[allow(clippy::similar_names)]
fn parse_papx_grpprl(grpprl: &[u8]) -> (ParagraphProperties, bool, bool) {
    let mut ppr = ParagraphProperties::default();
    let mut in_table = false;
    let mut is_row_end = false;
    let mut pos = 0usize;

    while pos + 2 <= grpprl.len() {
        let sprm = read_u16(grpprl, pos);
        pos += 2;
        let spra = (sprm >> 13) & 0x07;
        let op_len = match spra {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            6 => {
                if pos >= grpprl.len() {
                    break;
                }
                let l = usize::from(grpprl[pos]);
                pos += 1;
                l
            }
            _ => break,
        };
        if pos + op_len > grpprl.len() {
            break;
        }
        let op = &grpprl[pos..pos + op_len];
        pos += op_len;

        match sprm {
            0x2403 | 0x2461 => {
                ppr.alignment = Some(match op[0] {
                    1 => WordAlignment::Center,
                    2 => WordAlignment::Right,
                    3 | 4 => WordAlignment::Justify,
                    _ => WordAlignment::Left,
                });
            }
            0x840E | 0x845E if op.len() >= 2 => {
                let dxa = f32::from(read_i16(op, 0));
                ppr.indent_left_pt = Some(dxa / 20.0);
            }
            0x840F | 0x845D if op.len() >= 2 => {
                let dxa = f32::from(read_i16(op, 0));
                ppr.indent_right_pt = Some(dxa / 20.0);
            }
            0x8411 | 0x8460 if op.len() >= 2 => {
                let dxa = f32::from(read_i16(op, 0));
                ppr.indent_first_line_pt = Some(dxa / 20.0);
            }
            0xA413 if op.len() >= 2 => {
                let dya = f32::from(read_u16(op, 0));
                ppr.space_before_pt = Some(dya / 20.0);
            }
            0xA414 if op.len() >= 2 => {
                let dya = f32::from(read_u16(op, 0));
                ppr.space_after_pt = Some(dya / 20.0);
            }
            0x6412 if op.len() >= 4 => {
                let dya_line = read_i16(op, 0);
                let f_mult = read_i16(op, 2);
                if f_mult != 0 && dya_line > 0 {
                    ppr.line_spacing = Some(WordLineSpacing::Auto(f32::from(dya_line) / 240.0));
                } else if dya_line < 0 {
                    ppr.line_spacing = Some(WordLineSpacing::ExactPt(f32::from(-dya_line) / 20.0));
                } else if dya_line > 0 {
                    ppr.line_spacing = Some(WordLineSpacing::AtLeastPt(f32::from(dya_line) / 20.0));
                }
            }
            0x2405 => ppr.keep_lines = Some(op[0] != 0),
            0x2406 => ppr.keep_next = Some(op[0] != 0),
            0x2407 => ppr.page_break_before = Some(op[0] != 0),
            0x2416 | 0x244B => in_table = op[0] != 0,
            0x2417 | 0x244C => {
                in_table = true;
                is_row_end = op[0] != 0;
            }
            _ => {}
        }
    }

    (ppr, in_table, is_row_end)
}

#[allow(clippy::too_many_lines)]
/// File-offset range with formatting, decoded from an FKP.
trait FcInterval {
    fn fc_range(&self) -> (u32, u32);
}

impl FcInterval for ChpxInterval {
    fn fc_range(&self) -> (u32, u32) {
        (self.fc_start, self.fc_end)
    }
}

impl FcInterval for PapxInterval {
    fn fc_range(&self) -> (u32, u32) {
        (self.fc_start, self.fc_end)
    }
}

/// Interval containing `fc` in `runs` (sorted by start).
fn find_interval<T: FcInterval>(runs: &[T], fc: u32) -> Option<&T> {
    let idx = runs.partition_point(|r| r.fc_range().0 <= fc);
    let run = runs.get(idx.checked_sub(1)?)?;
    (fc < run.fc_range().1).then_some(run)
}

/// Match field marks in one pass: each closed begin mark (`0x13`) maps to its separator
/// (`0x14`, if any) and end mark (`0x15`). Unclosed begin marks are absent.
fn match_field_marks(chars: &[char]) -> BTreeMap<usize, (Option<usize>, usize)> {
    let mut open: Vec<(usize, Option<usize>)> = Vec::new();
    let mut matched = BTreeMap::new();
    for (i, &ch) in chars.iter().enumerate() {
        match ch {
            '\u{0013}' => open.push((i, None)),
            '\u{0014}' => {
                if let Some(top) = open.last_mut()
                    && top.1.is_none()
                {
                    top.1 = Some(i);
                }
            }
            '\u{0015}' => {
                if let Some((begin, sep)) = open.pop() {
                    matched.insert(begin, (sep, i));
                }
            }
            _ => {}
        }
    }
    matched
}

fn build_blocks_from_cp_slice(
    chars: &[char],
    char_fcs: &[u32],
    chpx_runs: &[ChpxInterval],
    papx_runs: &[PapxInterval],
    doc: &mut WordDocument,
) -> Vec<WordBlock> {
    let mut blocks = Vec::new();
    let mut pending_table_rows: Vec<WordTableRow> = Vec::new();
    let mut pending_row_cells: Vec<WordTableCell> = Vec::new();
    let mut pending_cell_blocks: Vec<WordBlock> = Vec::new();

    let mut start = 0usize;
    for i in 0..chars.len() {
        let ch = chars[i];
        let is_para_end = matches!(ch, '\r' | '\u{0007}' | '\u{000C}');
        if !is_para_end && i + 1 < chars.len() {
            continue;
        }
        let end = if is_para_end { i } else { i + 1 };
        let para_chars = &chars[start..end];
        let para_fcs = &char_fcs[start..end];
        let end_fc = char_fcs.get(i).copied().unwrap_or(0);

        let papx = find_interval(papx_runs, end_fc);
        let p_props = papx.map(|p| p.props.clone()).unwrap_or_default();
        let in_table = papx.is_some_and(|p| p.in_table) || ch == '\u{0007}';
        let is_row_end = papx.is_some_and(|p| p.is_row_end);

        let mut content = build_paragraph_contents(para_chars, para_fcs, chpx_runs, doc, 0);
        if ch == '\u{000C}' {
            content.push(ParagraphContent::Run(WordRun {
                props: RunProperties::default(),
                items: vec![RunItem::PageBreak],
                revision: None,
            }));
        }

        let para = WordParagraph {
            props: p_props,
            content,
        };

        if in_table {
            if is_row_end && para.content.is_empty() {
                if !pending_cell_blocks.is_empty() {
                    pending_row_cells.push(WordTableCell {
                        blocks: std::mem::take(&mut pending_cell_blocks),
                        ..WordTableCell::default()
                    });
                }
                if !pending_row_cells.is_empty() {
                    pending_table_rows.push(WordTableRow {
                        cells: std::mem::take(&mut pending_row_cells),
                        ..WordTableRow::default()
                    });
                }
            } else if ch == '\u{0007}' {
                pending_cell_blocks.push(WordBlock::Paragraph(para));
                pending_row_cells.push(WordTableCell {
                    blocks: std::mem::take(&mut pending_cell_blocks),
                    ..WordTableCell::default()
                });
                if is_row_end {
                    pending_table_rows.push(WordTableRow {
                        cells: std::mem::take(&mut pending_row_cells),
                        ..WordTableRow::default()
                    });
                }
            } else {
                pending_cell_blocks.push(WordBlock::Paragraph(para));
            }
        } else {
            flush_pending_table(
                &mut pending_cell_blocks,
                &mut pending_row_cells,
                &mut pending_table_rows,
                &mut blocks,
            );
            blocks.push(WordBlock::Paragraph(para));
        }

        start = i + 1;
    }

    flush_pending_table(
        &mut pending_cell_blocks,
        &mut pending_row_cells,
        &mut pending_table_rows,
        &mut blocks,
    );

    blocks
}

fn flush_pending_table(
    pending_cell_blocks: &mut Vec<WordBlock>,
    pending_row_cells: &mut Vec<WordTableCell>,
    pending_table_rows: &mut Vec<WordTableRow>,
    blocks: &mut Vec<WordBlock>,
) {
    if !pending_cell_blocks.is_empty() {
        pending_row_cells.push(WordTableCell {
            blocks: std::mem::take(pending_cell_blocks),
            ..WordTableCell::default()
        });
    }
    if !pending_row_cells.is_empty() {
        pending_table_rows.push(WordTableRow {
            cells: std::mem::take(pending_row_cells),
            ..WordTableRow::default()
        });
    }
    if !pending_table_rows.is_empty() {
        blocks.push(WordBlock::Table(WordTable {
            props: TableProperties::default(),
            grid_col_widths_pt: Vec::new(),
            rows: std::mem::take(pending_table_rows),
        }));
    }
}

#[allow(clippy::too_many_lines)]
fn build_paragraph_contents(
    chars: &[char],
    fcs: &[u32],
    chpx_runs: &[ChpxInterval],
    doc: &mut WordDocument,
    depth: usize,
) -> Vec<ParagraphContent> {
    let mut out = Vec::new();
    let mut i = 0usize;
    // Past the depth limit the marks are skipped as control characters and the text is kept.
    let fields = if depth < MAX_FIELD_DEPTH {
        match_field_marks(chars)
    } else {
        BTreeMap::new()
    };

    while i < chars.len() {
        let ch = chars[i];

        // Handle inline Word field markers: 0x13 (begin) ... 0x14 (sep) ... 0x15 (end)
        if ch == '\u{0013}'
            && let Some(&(sep_pos, e_idx)) = fields.get(&i)
        {
            let instr_end = sep_pos.unwrap_or(e_idx);
            let instr: String = chars[i + 1..instr_end]
                .iter()
                .filter(|&&c| c >= ' ' || c == '\t')
                .collect();
            let cached_result = if let Some(s_idx) = sep_pos {
                build_paragraph_contents(
                    &chars[s_idx + 1..e_idx],
                    &fcs[s_idx + 1..e_idx],
                    chpx_runs,
                    doc,
                    depth + 1,
                )
            } else {
                Vec::new()
            };

            let kind = classify_field_instruction(&instr, &mut doc.forensic);
            if let FieldKind::Hyperlink(ref url) = kind {
                if !url.is_empty() && !url.starts_with('#') {
                    doc.forensic.external_relationships.push(ExternalReference {
                        id: "field".to_string(),
                        ref_type: "hyperlink".to_string(),
                        target: url.clone(),
                    });
                }
                out.push(ParagraphContent::Hyperlink(WordHyperlink {
                    target: url.clone(),
                    tooltip: None,
                    content: cached_result,
                }));
            } else {
                out.push(ParagraphContent::Field(WordField {
                    instruction: instr.trim().to_string(),
                    kind,
                    cached_result,
                    locked: false,
                    dirty: false,
                }));
            }

            i = e_idx + 1;
            continue;
        }

        // Skip stray control characters (< 0x20 except \t and \x0B soft line break)
        if (ch as u32) < 0x20 && ch != '\t' && ch != '\u{000B}' {
            i += 1;
            continue;
        }

        let fc = fcs.get(i).copied().unwrap_or(0);
        let chpx = find_interval(chpx_runs, fc);
        let r_props = chpx.map(|c| c.props.clone()).unwrap_or_default();
        let is_del = chpx.is_some_and(|c| c.is_deleted);
        let is_ins = chpx.is_some_and(|c| c.is_inserted);

        // Coalesce consecutive characters sharing the same CHPX interval
        let mut items = Vec::new();
        let mut text_buf = String::new();
        while i < chars.len() {
            let c = chars[i];
            if c == '\u{0013}' || ((c as u32) < 0x20 && c != '\t' && c != '\u{000B}') {
                break;
            }
            let cur_fc = fcs.get(i).copied().unwrap_or(0);
            let same_chpx = match chpx {
                Some(interval) => cur_fc >= interval.fc_start && cur_fc < interval.fc_end,
                None => find_interval(chpx_runs, cur_fc).is_none(),
            };
            if !same_chpx {
                break;
            }

            if c == '\t' {
                if !text_buf.is_empty() {
                    items.push(RunItem::Text(std::mem::take(&mut text_buf)));
                }
                items.push(RunItem::Tab);
            } else if c == '\u{000B}' {
                if !text_buf.is_empty() {
                    items.push(RunItem::Text(std::mem::take(&mut text_buf)));
                }
                items.push(RunItem::LineBreak);
            } else {
                text_buf.push(c);
            }
            i += 1;
        }
        if !text_buf.is_empty() {
            items.push(RunItem::Text(text_buf));
        }

        if !items.is_empty() {
            let revision = if is_del || is_ins {
                doc.forensic.revision_count += 1;
                Some(WordRevisionMeta {
                    id: "0".to_string(),
                    kind: if is_del {
                        RevisionKind::Deleted
                    } else {
                        RevisionKind::Inserted
                    },
                    author: None,
                    date: None,
                })
            } else {
                None
            };
            out.push(ParagraphContent::Run(WordRun {
                props: r_props,
                items,
                revision,
            }));
        }
    }

    out
}

#[allow(clippy::if_not_else)]
fn parse_word2_raw(data: &[u8]) -> WordDocument {
    let mut doc = WordDocument::default();
    let fc_min = if data.len() >= 28 {
        (read_u32(data, 24) as usize).clamp(32, data.len())
    } else {
        data.len().min(128)
    };
    let fc_mac = if data.len() >= 32 {
        (read_u32(data, 28) as usize).clamp(fc_min, data.len())
    } else {
        data.len()
    };
    let slice = if fc_mac > fc_min {
        &data[fc_min..fc_mac]
    } else {
        &data[fc_min..]
    };

    let mut blocks = Vec::new();
    let mut cur_line = String::new();
    for &b in slice {
        if b == b'\r' || b == b'\n' {
            if !cur_line.trim().is_empty() {
                blocks.push(WordBlock::Paragraph(WordParagraph {
                    props: ParagraphProperties::default(),
                    content: vec![ParagraphContent::Run(WordRun {
                        props: RunProperties::default(),
                        items: vec![RunItem::Text(std::mem::take(&mut cur_line))],
                        revision: None,
                    })],
                }));
            } else {
                cur_line.clear();
            }
        } else if b >= 0x20 || b == b'\t' {
            cur_line.push(decode_cp1252_byte(b));
        }
    }
    if !cur_line.trim().is_empty() {
        blocks.push(WordBlock::Paragraph(WordParagraph {
            props: ParagraphProperties::default(),
            content: vec![ParagraphContent::Run(WordRun {
                props: RunProperties::default(),
                items: vec![RunItem::Text(cur_line)],
                revision: None,
            })],
        }));
    }

    doc.sections.push(WordSection {
        props: SectionProperties::default(),
        blocks,
    });
    doc
}

fn read_cfb_stream(
    comp: &mut CompoundFile<Cursor<&[u8]>>,
    name: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let stream = comp
        .open_stream(name)
        .or_else(|_| comp.open_stream(format!("/{name}")))
        .map_err(|e| Error::ParseError(format!("Missing CFB stream '{name}': {e}")))?;
    let mut buf = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    stream
        .take(limit)
        .read_to_end(&mut buf)
        .map_err(|e| Error::ParseError(format!("Failed reading CFB stream '{name}': {e}")))?;
    if buf.len() > max_bytes {
        return Err(Error::InvalidInput(format!(
            "CFB stream '{name}' exceeds maximum size ({max_bytes} bytes)"
        )));
    }
    Ok(buf)
}

fn toggle_from_sprm_byte(val: u8) -> ToggleProp {
    if val == 0 {
        ToggleProp::Off
    } else {
        ToggleProp::Toggle
    }
}

fn ico_color_hex(ico: u8) -> Option<&'static str> {
    match ico {
        1 => Some("000000"),
        2 => Some("0000FF"),
        3 => Some("00FFFF"),
        4 => Some("00FF00"),
        5 => Some("FF00FF"),
        6 => Some("FF0000"),
        7 => Some("FFFF00"),
        8 => Some("FFFFFF"),
        9 => Some("000080"),
        10 => Some("008080"),
        11 => Some("008000"),
        12 => Some("800080"),
        13 => Some("800000"),
        14 => Some("808000"),
        15 => Some("808080"),
        16 => Some("C0C0C0"),
        _ => None,
    }
}

fn decode_cp1252_byte(b: u8) -> char {
    match b {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        other => char::from(other),
    }
}

fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_i16(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_synthetic_doc(encrypted: bool, text: &str) -> Vec<u8> {
        build_synthetic_doc_with(encrypted, text, |_| {})
    }

    /// `customize` edits the 512-byte FIB after the defaults are written.
    fn build_synthetic_doc_with(
        encrypted: bool,
        text: &str,
        customize: impl FnOnce(&mut [u8]),
    ) -> Vec<u8> {
        let mut comp = CompoundFile::create(Cursor::new(Vec::new())).unwrap();

        // Build WordDocument stream: 512-byte FIB + compressed CP-1252 text at offset 512
        let mut ws = vec![0u8; 512];
        ws[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes()); // wIdent
        ws[2..4].copy_from_slice(&0x00C1u16.to_le_bytes()); // nFib (Word 97)
        let mut flags1 = 0x0200u16; // fWhichTblStm = 1 ("1Table")
        if encrypted {
            flags1 |= 0x0100; // fEncrypted
        }
        ws[10..12].copy_from_slice(&flags1.to_le_bytes());
        ws[24..28].copy_from_slice(&512u32.to_le_bytes()); // fcMin

        let text_bytes = text.as_bytes();
        #[allow(clippy::cast_possible_truncation)]
        let fc_mac = (512 + text_bytes.len()) as u32;
        ws[28..32].copy_from_slice(&fc_mac.to_le_bytes());
        ws[32..34].copy_from_slice(&14u16.to_le_bytes()); // csw = 14
        ws[62..64].copy_from_slice(&22u16.to_le_bytes()); // clw = 22
        #[allow(clippy::cast_possible_truncation)]
        let ccp_text = text_bytes.len() as u32;
        ws[76..80].copy_from_slice(&ccp_text.to_le_bytes()); // ccpText

        // cbRgFcLcb at offset 152
        ws[152..154].copy_from_slice(&93u16.to_le_bytes());
        // Pair 33 (fcClx, lcbClx) at 154 + 33 * 8 = 418
        ws[418..422].copy_from_slice(&0u32.to_le_bytes()); // fcClx = 0
        ws[422..426].copy_from_slice(&21u32.to_le_bytes()); // lcbClx = 21
        customize(&mut ws);

        ws.extend_from_slice(text_bytes);

        // Build 1Table stream with a valid Clx (Pcdt: clxt=2, lcb=16, CP[0]=0, CP[1]=ccp_text, Pcd with compressed fc)
        let mut tbl = Vec::new();
        tbl.push(2u8); // clxt = Pcdt
        tbl.extend_from_slice(&16u32.to_le_bytes()); // lcb = 16
        tbl.extend_from_slice(&0u32.to_le_bytes()); // CP[0] = 0
        tbl.extend_from_slice(&ccp_text.to_le_bytes()); // CP[1] = ccp_text
        tbl.extend_from_slice(&0u16.to_le_bytes()); // Pcd flags
        let fc_compressed = (512u32 * 2) | 0x4000_0000;
        tbl.extend_from_slice(&fc_compressed.to_le_bytes());
        tbl.extend_from_slice(&0u16.to_le_bytes()); // prm

        {
            let mut s = comp.create_stream("WordDocument").unwrap();
            s.write_all(&ws).unwrap();
        }
        {
            let mut s = comp.create_stream("1Table").unwrap();
            s.write_all(&tbl).unwrap();
        }

        comp.into_inner().into_inner()
    }

    #[test]
    fn parses_clx_piece_table_and_fields_from_doc() {
        let raw = build_synthetic_doc(
            false,
            "First paragraph\rSecond with \u{0013} HYPERLINK \"https://example.com\" \u{0014}Example\u{0015}\r",
        );
        assert!(is_doc_bytes(&raw));
        let doc = parse_doc(&raw, DEFAULT_MAX_DOC_STREAM_BYTES).unwrap();
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].blocks.len(), 2);
        assert_eq!(doc.forensic.external_relationships.len(), 1);
        assert_eq!(
            doc.forensic.external_relationships[0].target,
            "https://example.com"
        );
    }

    fn doc_text(doc: &WordDocument) -> String {
        crate::udm::word_to_udm(doc, "DOC").extract_text()
    }

    #[test]
    fn deeply_nested_and_unclosed_fields_stay_bounded() {
        let nested = format!(
            "{}inner{}\r",
            "\u{0013}x\u{0014}".repeat(20_000),
            "\u{0015}".repeat(20_000)
        );
        let raw = build_synthetic_doc(false, &nested);
        let doc = parse_doc(&raw, DEFAULT_MAX_DOC_STREAM_BYTES).unwrap();
        assert!(doc_text(&doc).contains("inner"));

        let start = std::time::Instant::now();
        let unclosed = format!("{}tail\r", "\u{0013}".repeat(100_000));
        let raw = build_synthetic_doc(false, &unclosed);
        let doc = parse_doc(&raw, DEFAULT_MAX_DOC_STREAM_BYTES).unwrap();
        assert!(doc_text(&doc).contains("tail"));
        assert!(start.elapsed().as_secs() < 5);
    }

    #[test]
    fn word95_fib_is_left_to_the_fallback() {
        let raw = build_synthetic_doc_with(false, "Text\r", |ws| {
            ws[2..4].copy_from_slice(&0x0068u16.to_le_bytes()); // nFib (Word 95)
        });
        let err = parse_doc(&raw, DEFAULT_MAX_DOC_STREAM_BYTES).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFormat(_)));
    }

    #[test]
    fn footnote_story_text_is_kept() {
        let raw = build_synthetic_doc_with(false, "Body text\rFootnote text\r", |ws| {
            ws[76..80].copy_from_slice(&10u32.to_le_bytes()); // ccpText: "Body text\r"
            ws[80..84].copy_from_slice(&14u32.to_le_bytes()); // ccpFtn
        });
        let doc = parse_doc(&raw, DEFAULT_MAX_DOC_STREAM_BYTES).unwrap();
        assert_eq!(doc.sections[0].blocks.len(), 1);
        assert_eq!(doc.unplaced_stories.len(), 1);
        assert_eq!(doc.unplaced_stories[0].id, "footnotes");
        let text = doc_text(&doc);
        assert!(text.contains("Body text") && text.contains("Footnote text"));
    }

    #[test]
    fn encrypted_doc_returns_encrypted_document_error() {
        let raw = build_synthetic_doc(true, "Secret text\r");
        let err = parse_doc(&raw, DEFAULT_MAX_DOC_STREAM_BYTES).unwrap_err();
        assert!(matches!(err, Error::Encrypted(_)));
    }
}
