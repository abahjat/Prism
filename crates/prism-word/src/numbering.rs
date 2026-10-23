// SPDX-License-Identifier: AGPL-3.0-only
//! Word list and numbering state machine (`w:numbering`, `abstractNum`, `num`, `lvl`, `lvlOverride`)
//! and Symbol/Wingdings character mapping.

use crate::model::{
    LevelDef, LevelSuffix, NumberFormat, NumberingRef, RunProperties, WordNumbering,
};
use std::collections::BTreeMap;

/// Formatted list marker produced for a numbered or bulleted paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct FormattedListMarker {
    /// Display string of the marker (e.g. `"1."`, `"I.A."`, `"•"`).
    pub text: String,
    /// Suffix following the marker (`Tab`, `Space`, or `Nothing`).
    pub suffix: LevelSuffix,
    /// Run properties defined on the numbering level (`w:lvl/w:rPr`).
    pub run_props: RunProperties,
}

/// Stateful list counter evaluator across a [`WordDocument`](crate::model::WordDocument).
#[derive(Debug, Clone, Default)]
pub struct NumberingTracker {
    /// Current counter value for `(num_id, ilvl)`.
    counters: BTreeMap<(u32, u8), i32>,
}

impl NumberingTracker {
    /// Create a new [`NumberingTracker`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the counter for `num_ref` in `numbering` and format its marker string.
    #[must_use]
    #[allow(clippy::similar_names)]
    pub fn next_marker(
        &mut self,
        num_ref: NumberingRef,
        numbering: &WordNumbering,
    ) -> Option<FormattedListMarker> {
        let num_id = num_ref.num_id;
        let ilvl = num_ref.ilvl;
        let lvl = numbering.resolve_level(num_id, ilvl)?;

        // Determine starting value for this level
        let start_val = numbering.start_override(num_id, ilvl).unwrap_or(lvl.start);

        // Increment or initialize current level
        let next_val = match self.counters.get(&(num_id, ilvl)).copied() {
            Some(prev) => prev.saturating_add(1),
            None => start_val,
        };
        self.counters.insert((num_id, ilvl), next_val);

        // Reset deeper levels whose restart_after_level >= ilvl
        for deeper in ilvl.saturating_add(1)..=8 {
            let should_restart = numbering.resolve_level(num_id, deeper).is_none_or(|dl| {
                dl.restart_after_level.unwrap_or(deeper.saturating_sub(1)) >= ilvl
            });
            if should_restart {
                self.counters.remove(&(num_id, deeper));
            }
        }

        // Format `lvl_text` template (`%1`, `%2`, ..., `%9`)
        let text = if lvl.num_fmt == NumberFormat::None {
            String::new()
        } else if lvl.num_fmt == NumberFormat::Bullet {
            format_bullet_text(&lvl.lvl_text, lvl.run_props.fonts.ascii.as_deref())
        } else {
            self.expand_level_template(num_id, ilvl, lvl, numbering)
        };

        let mut run_props = lvl.run_props.clone();
        // If this is a bullet in Symbol / Wingdings, clear the symbol font override so the mapped
        // Unicode bullet glyph shapes with the bundled text font.
        if lvl.num_fmt == NumberFormat::Bullet
            && run_props.fonts.ascii.as_deref().is_some_and(is_symbol_font)
        {
            run_props.fonts.ascii = None;
            run_props.fonts.h_ansi = None;
        }

        Some(FormattedListMarker {
            text,
            suffix: lvl.suffix,
            run_props,
        })
    }

    #[allow(clippy::similar_names)]
    fn expand_level_template(
        &self,
        num_id: u32,
        current_ilvl: u8,
        lvl: &LevelDef,
        numbering: &WordNumbering,
    ) -> String {
        if lvl.lvl_text.is_empty() {
            let val = self
                .counters
                .get(&(num_id, current_ilvl))
                .copied()
                .unwrap_or(1);
            return format_number(val, &lvl.num_fmt);
        }

        let mut out = String::new();
        let mut chars = lvl.lvl_text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%'
                && let Some(&digit) = chars.peek()
                && ('1'..='9').contains(&digit)
            {
                chars.next();
                let target_ilvl = (digit as u8) - b'1';
                let target_lvl = numbering.resolve_level(num_id, target_ilvl);
                let default_start = numbering
                    .start_override(num_id, target_ilvl)
                    .or_else(|| target_lvl.map(|l| l.start))
                    .unwrap_or(1);
                let val = self
                    .counters
                    .get(&(num_id, target_ilvl))
                    .copied()
                    .unwrap_or(default_start);
                let fmt = if lvl.is_legal && target_ilvl < current_ilvl {
                    &NumberFormat::Decimal
                } else {
                    target_lvl.map_or(&lvl.num_fmt, |l| &l.num_fmt)
                };
                out.push_str(&format_number(val, fmt));
            } else {
                out.push(ch);
            }
        }
        out
    }
}

/// Format an integer value using the specified [`NumberFormat`].
#[must_use]
pub fn format_number(val: i32, fmt: &NumberFormat) -> String {
    let n = val.max(0);
    match fmt {
        NumberFormat::Decimal | NumberFormat::Custom(_) => n.to_string(),
        NumberFormat::DecimalZero => format!("{n:02}"),
        NumberFormat::UpperRoman => to_roman(n, true),
        NumberFormat::LowerRoman => to_roman(n, false),
        NumberFormat::UpperLetter => to_alpha(n, true),
        NumberFormat::LowerLetter => to_alpha(n, false),
        NumberFormat::Ordinal => to_ordinal(n),
        NumberFormat::Bullet => "•".to_string(),
        NumberFormat::None => String::new(),
    }
}

/// Largest value written in roman numerals (32 `M`s); Word's list start values stop at 32767.
/// Larger stored values fall back to decimal so a marker stays short.
const MAX_ROMAN_VALUE: i32 = 32_767;

/// Letter markers repeat the letter `(n - 1) / 26 + 1` times; Word cycles back to `a` after
/// `zzz…` (30 letters, value 780), which also bounds the marker length.
const LETTER_CYCLE: i32 = 26 * 30;

fn to_roman(mut n: i32, upper: bool) -> String {
    if n <= 0 || n > MAX_ROMAN_VALUE {
        return n.to_string();
    }
    let table = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for &(value, numeral) in &table {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    if upper { out } else { out.to_ascii_lowercase() }
}

fn to_alpha(n: i32, upper: bool) -> String {
    if n <= 0 {
        return n.to_string();
    }
    // Word's upperLetter/lowerLetter repeats the letter: 1=A..26=Z, 27=AA, 28=BB...
    let n = (n - 1) % LETTER_CYCLE + 1;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let idx = ((n - 1) % 26) as u8;
    #[allow(clippy::cast_sign_loss)]
    let count = (((n - 1) / 26) + 1) as usize;
    let base = if upper { b'A' } else { b'a' };
    let ch = char::from(base + idx);
    std::iter::repeat_n(ch, count).collect()
}

fn to_ordinal(n: i32) -> String {
    let rem100 = n.abs() % 100;
    let rem10 = n.abs() % 10;
    let suffix = if (11..=13).contains(&rem100) {
        "th"
    } else {
        match rem10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

fn is_symbol_font(font: &str) -> bool {
    let lower = font.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "symbol" | "wingdings" | "wingdings 2" | "wingdings 3" | "webdings"
    )
}

fn format_bullet_text(lvl_text: &str, font: Option<&str>) -> String {
    if lvl_text.is_empty() {
        return "•".to_string();
    }
    lvl_text
        .chars()
        .map(|ch| map_symbol_char(font.unwrap_or("Symbol"), ch))
        .collect()
}

/// Map a legacy Symbol / Wingdings / PUA character code to its standard Unicode counterpart.
#[must_use]
#[allow(clippy::match_same_arms)]
pub fn map_symbol_char(font: &str, ch: char) -> char {
    let code = ch as u32;
    // Strip Microsoft Private Use Area prefix (0xF000..=0xF0FF -> 0x00..=0xFF)
    let low = if (0xF000..=0xF0FF).contains(&code) {
        code - 0xF000
    } else {
        code
    };

    let lower_font = font.to_ascii_lowercase();
    if lower_font.starts_with("wingdings") || lower_font == "webdings" {
        return match low {
            0x6C | 0x9F => '•',        // Solid circle
            0x6E | 0xA7 => '▪',        // Solid square
            0x71 | 0x72 => '❑',        // Hollow square
            0x75 | 0x76 => '❖',        // Diamond
            0xD8 | 0xE0 | 0xE8 => '➢', // Arrowhead
            0xFC | 0xFE => '✓',        // Checkmark
            0xFB | 0xFD => '✗',        // Cross
            _ => '•',
        };
    }

    if lower_font == "symbol" || (0xF000..=0xF0FF).contains(&code) {
        return match low {
            0xB7 | 0x2022 => '•',
            0x6F | 0xB0 => '◦',
            0xA7 => '▪',
            0xAE | 0xDE => '→',
            0xAC | 0xDC => '←',
            0xAD | 0xDD => '↑',
            0xAF | 0xDF => '↓',
            0xB1 => '±',
            0xB4 => '×',
            0xB8 => '÷',
            0xB9 => '≠',
            0xA3 => '≤',
            0xB3 => '≥',
            0xA5 => '∞',
            _ => '•',
        };
    }

    // Courier New 'o' bullet convention in Word
    if ch == 'o' && lower_font == "courier new" {
        return '◦';
    }

    ch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AbstractNum, LevelDef, LevelOverride, NumInstance, ParagraphProperties, WordAlignment,
    };

    #[test]
    fn multi_level_numbering_and_restart_and_legal() {
        let mut numbering = WordNumbering::default();
        let mut abs = AbstractNum {
            id: 0,
            num_style_link: None,
            levels: BTreeMap::new(),
        };
        abs.levels.insert(
            0,
            LevelDef {
                ilvl: 0,
                start: 1,
                num_fmt: NumberFormat::UpperRoman,
                lvl_text: "%1.".to_string(),
                alignment: WordAlignment::Left,
                suffix: LevelSuffix::Tab,
                is_legal: false,
                restart_after_level: None,
                para_style_id: None,
                para_props: ParagraphProperties::default(),
                run_props: RunProperties::default(),
            },
        );
        abs.levels.insert(
            1,
            LevelDef {
                ilvl: 1,
                start: 1,
                num_fmt: NumberFormat::Decimal,
                lvl_text: "%1.%2.".to_string(),
                alignment: WordAlignment::Left,
                suffix: LevelSuffix::Tab,
                is_legal: true, // Forces %1 to format as Arabic decimal ("1.1." instead of "I.1.")
                restart_after_level: Some(0),
                para_style_id: None,
                para_props: ParagraphProperties::default(),
                run_props: RunProperties::default(),
            },
        );
        numbering.abstract_nums.insert(0, abs);
        numbering.nums.insert(
            1,
            NumInstance {
                num_id: 1,
                abstract_num_id: 0,
                overrides: BTreeMap::new(),
            },
        );

        let mut tracker = NumberingTracker::new();
        assert_eq!(
            tracker
                .next_marker(NumberingRef { num_id: 1, ilvl: 0 }, &numbering)
                .unwrap()
                .text,
            "I."
        );
        assert_eq!(
            tracker
                .next_marker(NumberingRef { num_id: 1, ilvl: 1 }, &numbering)
                .unwrap()
                .text,
            "1.1."
        );
        assert_eq!(
            tracker
                .next_marker(NumberingRef { num_id: 1, ilvl: 1 }, &numbering)
                .unwrap()
                .text,
            "1.2."
        );
        // Increment level 0 -> resets level 1!
        assert_eq!(
            tracker
                .next_marker(NumberingRef { num_id: 1, ilvl: 0 }, &numbering)
                .unwrap()
                .text,
            "II."
        );
        assert_eq!(
            tracker
                .next_marker(NumberingRef { num_id: 1, ilvl: 1 }, &numbering)
                .unwrap()
                .text,
            "2.1."
        );
    }

    #[test]
    fn large_stored_values_give_short_markers() {
        // A start value near i32::MAX once produced an 82-million-letter marker.
        assert!(format_number(i32::MAX, &NumberFormat::LowerLetter).len() <= 30);
        assert_eq!(
            format_number(780, &NumberFormat::LowerLetter),
            "z".repeat(30)
        );
        assert_eq!(format_number(781, &NumberFormat::LowerLetter), "a");
        assert_eq!(
            format_number(i32::MAX, &NumberFormat::UpperRoman),
            "2147483647"
        );
        assert_eq!(format_number(1999, &NumberFormat::UpperRoman), "MCMXCIX");
    }

    #[test]
    fn out_of_range_level_does_not_overflow() {
        let mut numbering = WordNumbering::default();
        let mut abs = AbstractNum {
            id: 0,
            num_style_link: None,
            levels: BTreeMap::new(),
        };
        abs.levels.insert(
            255,
            LevelDef {
                ilvl: 255,
                start: 1,
                num_fmt: NumberFormat::Decimal,
                lvl_text: "%1.".to_string(),
                alignment: WordAlignment::Left,
                suffix: LevelSuffix::Tab,
                is_legal: false,
                restart_after_level: None,
                para_style_id: None,
                para_props: ParagraphProperties::default(),
                run_props: RunProperties::default(),
            },
        );
        numbering.abstract_nums.insert(0, abs);
        numbering.nums.insert(
            1,
            NumInstance {
                num_id: 1,
                abstract_num_id: 0,
                overrides: BTreeMap::new(),
            },
        );
        let mut tracker = NumberingTracker::new();
        let num_ref = NumberingRef {
            num_id: 1,
            ilvl: 255,
        };
        assert!(tracker.next_marker(num_ref, &numbering).is_some());
    }

    #[test]
    fn start_override_and_bullet_mapping() {
        let mut numbering = WordNumbering::default();
        let mut abs = AbstractNum {
            id: 0,
            num_style_link: None,
            levels: BTreeMap::new(),
        };
        abs.levels.insert(
            0,
            LevelDef {
                ilvl: 0,
                start: 1,
                num_fmt: NumberFormat::UpperLetter,
                lvl_text: "(%1)".to_string(),
                alignment: WordAlignment::Left,
                suffix: LevelSuffix::Space,
                is_legal: false,
                restart_after_level: None,
                para_style_id: None,
                para_props: ParagraphProperties::default(),
                run_props: RunProperties::default(),
            },
        );
        numbering.abstract_nums.insert(0, abs);
        let mut overrides = BTreeMap::new();
        overrides.insert(
            0,
            LevelOverride {
                ilvl: 0,
                start_override: Some(3),
                level_def: None,
            },
        );
        numbering.nums.insert(
            5,
            NumInstance {
                num_id: 5,
                abstract_num_id: 0,
                overrides,
            },
        );

        let mut tracker = NumberingTracker::new();
        assert_eq!(
            tracker
                .next_marker(NumberingRef { num_id: 5, ilvl: 0 }, &numbering)
                .unwrap()
                .text,
            "(C)"
        );
        assert_eq!(map_symbol_char("Symbol", '\u{F0B7}'), '•');
        assert_eq!(map_symbol_char("Wingdings", '\u{F0FC}'), '✓');
    }
}
