// SPDX-License-Identifier: AGPL-3.0-only
//! Word style inheritance, toggle-property evaluation, and theme color/font resolution.
//!
//! Implements the inheritance hierarchy documented in ADR 0005 (`docs/adr/0005-word-style-inheritance-and-toggles.md`).

use crate::model::{
    FontSlots, LevelDef, ParagraphProperties, ResolvedParagraphStyle, ResolvedRunStyle,
    RunProperties, StyleType, TabStop, TabStopKind, WordBorders, WordNumbering, WordStyle,
    WordStyleSheet, WordTheme,
};

/// Maximum `w:basedOn` chain depth before terminating to prevent cyclic style `DoS`.
pub const MAX_STYLE_DEPTH: usize = 64;

/// Map a `WordprocessingML` `w:themeColor` attribute value to a `DrawingML` theme slot key.
#[must_use]
pub fn map_theme_color_slot(theme_color: &str) -> &str {
    match theme_color {
        "dark1" | "text1" | "dk1" => "dk1",
        "light1" | "background1" | "lt1" => "lt1",
        "dark2" | "text2" | "dk2" => "dk2",
        "light2" | "background2" | "lt2" => "lt2",
        "hyperlink" | "hlink" => "hlink",
        "followedHyperlink" | "folHlink" => "folHlink",
        other => other,
    }
}

/// Resolve a Word color specification (`w:val`, `w:themeColor`, `w:themeTint`, `w:themeShade`)
/// to a `#RRGGBB` string.
///
/// Priority:
/// 1. If `theme_color` resolves in `theme`, apply `theme_tint` or `theme_shade` ( per ECMA-376 §17.3.2.6).
/// 2. Otherwise, if `val` is present and not `"auto"` or `"none"`, return `#RRGGBB`.
#[must_use]
pub fn resolve_word_color(
    val: Option<&str>,
    theme_color: Option<&str>,
    theme_tint: Option<u8>,
    theme_shade: Option<u8>,
    theme: &WordTheme,
) -> Option<String> {
    if let Some(tc) = theme_color {
        let slot = map_theme_color_slot(tc);
        if let Some(base_hex) = theme.colors.get(slot)
            && let Some((r, g, b)) = parse_hex_rgb(base_hex)
        {
            let (r, g, b) = if let Some(tint) = theme_tint {
                (
                    apply_tint_channel(r, tint),
                    apply_tint_channel(g, tint),
                    apply_tint_channel(b, tint),
                )
            } else if let Some(shade) = theme_shade {
                (
                    apply_shade_channel(r, shade),
                    apply_shade_channel(g, shade),
                    apply_shade_channel(b, shade),
                )
            } else {
                (r, g, b)
            };
            return Some(format!("#{r:02X}{g:02X}{b:02X}"));
        }
    }

    if let Some(v) = val {
        let trimmed = v.trim().trim_start_matches('#');
        if !trimmed.is_empty()
            && !trimmed.eq_ignore_ascii_case("auto")
            && !trimmed.eq_ignore_ascii_case("none")
        {
            return Some(format!("#{}", trimmed.to_ascii_uppercase()));
        }
    }

    None
}

fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let clean = hex.trim().trim_start_matches('#');
    // `get` rather than indexing: the value comes from the document and may not be ASCII.
    let r = u8::from_str_radix(clean.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(clean.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(clean.get(4..6)?, 16).ok()?;
    Some((r, g, b))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn apply_tint_channel(channel: u8, tint: u8) -> u8 {
    // ECMA-376: tint of 0xFF = unchanged, smaller values move toward white (255)
    let ratio = f32::from(tint) / 255.0;
    let c = f32::from(channel);
    (c + (255.0 - c) * (1.0 - ratio)).round().clamp(0.0, 255.0) as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn apply_shade_channel(channel: u8, shade: u8) -> u8 {
    // ECMA-376: shade of 0xFF = unchanged, smaller values move toward black (0)
    let ratio = f32::from(shade) / 255.0;
    let c = f32::from(channel);
    (c * ratio).round().clamp(0.0, 255.0) as u8
}

/// Resolve a theme font slot name (`majorHAnsi`, `minorHAnsi`, `majorEastAsia`, etc.) against [`WordTheme`].
#[must_use]
pub fn resolve_theme_font(theme_slot: &str, theme: &WordTheme) -> Option<String> {
    match theme_slot {
        "majorHAnsi" | "majorAscii" => theme.major_latin.clone(),
        "minorHAnsi" | "minorAscii" => theme.minor_latin.clone(),
        "majorEastAsia" => theme
            .major_east_asia
            .clone()
            .or_else(|| theme.major_latin.clone()),
        "minorEastAsia" => theme
            .minor_east_asia
            .clone()
            .or_else(|| theme.minor_latin.clone()),
        "majorBidi" => theme.major_cs.clone().or_else(|| theme.major_latin.clone()),
        "minorBidi" => theme.minor_cs.clone().or_else(|| theme.minor_latin.clone()),
        _ => None,
    }
}

/// Resolve [`FontSlots`] into `(latin, east_asia, cs)` font family names using [`WordTheme`].
#[must_use]
pub fn resolve_font_slots(
    slots: &FontSlots,
    theme: &WordTheme,
) -> (Option<String>, Option<String>, Option<String>) {
    let latin = slots
        .ascii
        .clone()
        .or_else(|| slots.h_ansi.clone())
        .or_else(|| {
            slots
                .ascii_theme
                .as_deref()
                .and_then(|s| resolve_theme_font(s, theme))
        })
        .or_else(|| {
            slots
                .h_ansi_theme
                .as_deref()
                .and_then(|s| resolve_theme_font(s, theme))
        });

    let east_asia = slots.east_asia.clone().or_else(|| {
        slots
            .east_asia_theme
            .as_deref()
            .and_then(|s| resolve_theme_font(s, theme))
    });

    let cs = slots.cs.clone().or_else(|| {
        slots
            .cs_theme
            .as_deref()
            .and_then(|s| resolve_theme_font(s, theme))
    });

    (latin, east_asia, cs)
}

impl WordNumbering {
    /// Look up the effective [`LevelDef`] for `(num_id, ilvl)`, respecting any `w:lvlOverride`.
    #[must_use]
    pub fn resolve_level(&self, num_id: u32, ilvl: u8) -> Option<&LevelDef> {
        let num = self.nums.get(&num_id)?;
        if let Some(ov) = num.overrides.get(&ilvl)
            && let Some(ref lvl) = ov.level_def
        {
            return Some(lvl);
        }
        let abs = self.abstract_nums.get(&num.abstract_num_id)?;
        abs.levels.get(&ilvl)
    }

    /// Look up any `w:startOverride` for `(num_id, ilvl)`.
    #[must_use]
    pub fn start_override(&self, num_id: u32, ilvl: u8) -> Option<i32> {
        self.nums.get(&num_id)?.overrides.get(&ilvl)?.start_override
    }
}

impl WordStyleSheet {
    /// Collect the `basedOn` inheritance chain for a style from root ancestor to leaf style.
    ///
    /// Bounded to [`MAX_STYLE_DEPTH`] and cycle-safe.
    #[must_use]
    pub fn style_chain(
        &self,
        style_id: Option<&str>,
        default_for_type: Option<StyleType>,
    ) -> Vec<&WordStyle> {
        let start_id = style_id.or_else(|| {
            let target_type = default_for_type?;
            self.styles
                .values()
                .find(|s| s.style_type == target_type && s.is_default)
                .map(|s| s.id.as_str())
        });

        let Some(mut curr_id) = start_id else {
            return Vec::new();
        };

        let mut chain = Vec::new();
        let mut visited: Vec<&str> = Vec::new();

        while chain.len() < MAX_STYLE_DEPTH {
            if visited.contains(&curr_id) {
                break;
            }
            let Some(style) = self.styles.get(curr_id) else {
                break;
            };
            visited.push(&style.id);
            chain.push(style);
            match style.based_on.as_deref() {
                Some(parent_id) => curr_id = parent_id,
                None => break,
            }
        }

        chain.reverse();
        chain
    }

    /// Resolve effective paragraph formatting across `docDefaults`, paragraph `basedOn` chain,
    /// numbering level properties, and direct paragraph formatting.
    #[must_use]
    pub fn resolve_paragraph_style(
        &self,
        style_id: Option<&str>,
        numbering: Option<&WordNumbering>,
        direct: &ParagraphProperties,
    ) -> ResolvedParagraphStyle {
        let mut resolved = ResolvedParagraphStyle::default();
        let effective_style_id = style_id.or(direct.style_id.as_deref());
        resolved.style_id = effective_style_id.map(str::to_string);

        // 1. Document defaults
        apply_para_props(&mut resolved, &self.default_para_props);

        // 2. Paragraph style chain (root -> leaf)
        for style in self.style_chain(effective_style_id, Some(StyleType::Paragraph)) {
            apply_para_props(&mut resolved, &style.para_props);
        }

        // Determine if numbering applies (either from style or direct)
        let effective_num = direct.numbering.or(resolved.numbering);
        if let (Some(num_ref), Some(num_table)) = (effective_num, numbering)
            && let Some(lvl) = num_table.resolve_level(num_ref.num_id, num_ref.ilvl)
        {
            // 3. Numbering level paragraph properties
            apply_para_props(&mut resolved, &lvl.para_props);
        }

        // 4. Direct paragraph formatting (highest priority)
        apply_para_props(&mut resolved, direct);

        resolved
    }

    /// Resolve effective run formatting across `docDefaults`, paragraph style `basedOn` chain,
    /// character style `basedOn` chain, and direct run formatting.
    #[must_use]
    pub fn resolve_run_style(
        &self,
        para_style_id: Option<&str>,
        char_style_id: Option<&str>,
        direct: &RunProperties,
        theme: &WordTheme,
    ) -> ResolvedRunStyle {
        let mut resolved = ResolvedRunStyle::default();
        if let Some(ref minor) = theme.minor_latin {
            resolved.font_family.clone_from(minor);
        }

        // 1. Document defaults
        apply_run_props_in_style(&mut resolved, &self.default_run_props, theme);

        // 2. Paragraph style chain (root -> leaf)
        for style in self.style_chain(para_style_id, Some(StyleType::Paragraph)) {
            apply_run_props_in_style(&mut resolved, &style.run_props, theme);
        }

        // 3. Character style chain (root -> leaf)
        let effective_char_style = char_style_id.or(direct.style_id.as_deref());
        if effective_char_style.is_some() {
            for style in self.style_chain(effective_char_style, None) {
                apply_run_props_in_style(&mut resolved, &style.run_props, theme);
            }
        }

        // 4. Direct run properties (override semantics for toggles)
        apply_run_props_direct(&mut resolved, direct, theme);

        resolved
    }
}

fn apply_para_props(target: &mut ResolvedParagraphStyle, delta: &ParagraphProperties) {
    if let Some(align) = delta.alignment {
        target.alignment = align;
    }
    if let Some(sb) = delta.space_before_pt {
        target.space_before_pt = sb;
    }
    if let Some(sa) = delta.space_after_pt {
        target.space_after_pt = sa;
    }
    if let Some(ls) = delta.line_spacing {
        target.line_spacing = ls;
    }
    if let Some(cs) = delta.contextual_spacing {
        target.contextual_spacing = cs;
    }
    if let Some(left) = delta.indent_left_pt {
        target.indent_left_pt = left;
    }
    if let Some(right) = delta.indent_right_pt {
        target.indent_right_pt = right;
    }
    if let Some(first) = delta.indent_first_line_pt {
        target.indent_first_line_pt = first;
    }
    if let Some(kn) = delta.keep_next {
        target.keep_next = kn;
    }
    if let Some(kl) = delta.keep_lines {
        target.keep_lines = kl;
    }
    if let Some(pbb) = delta.page_break_before {
        target.page_break_before = pbb;
    }
    if let Some(wc) = delta.widow_control {
        target.widow_control = wc;
    }
    if let Some(ol) = delta.outline_level {
        target.outline_level = Some(ol);
    }
    if let Some(num) = delta.numbering {
        target.numbering = Some(num);
    }
    if !delta.tab_stops.is_empty() {
        merge_tab_stops(&mut target.tab_stops, &delta.tab_stops);
    }
    merge_borders(&mut target.borders, &delta.borders);
    if let Some(ref fill) = delta.shading_fill {
        target.shading_fill = Some(fill.clone());
    }
}

fn merge_tab_stops(existing: &mut Vec<TabStop>, delta: &[TabStop]) {
    for stop in delta {
        if stop.kind == TabStopKind::Clear {
            existing.retain(|s| (s.position_pt - stop.position_pt).abs() > 0.05);
        } else {
            existing.retain(|s| (s.position_pt - stop.position_pt).abs() > 0.05);
            existing.push(*stop);
        }
    }
    existing.sort_by(|a, b| a.position_pt.total_cmp(&b.position_pt));
}

fn merge_borders(target: &mut WordBorders, delta: &WordBorders) {
    if let Some(ref b) = delta.top {
        target.top = Some(b.clone());
    }
    if let Some(ref b) = delta.right {
        target.right = Some(b.clone());
    }
    if let Some(ref b) = delta.bottom {
        target.bottom = Some(b.clone());
    }
    if let Some(ref b) = delta.left {
        target.left = Some(b.clone());
    }
    if let Some(ref b) = delta.between {
        target.between = Some(b.clone());
    }
    if let Some(ref b) = delta.inside_h {
        target.inside_h = Some(b.clone());
    }
    if let Some(ref b) = delta.inside_v {
        target.inside_v = Some(b.clone());
    }
}

fn apply_run_props_in_style(
    target: &mut ResolvedRunStyle,
    delta: &RunProperties,
    theme: &WordTheme,
) {
    apply_non_toggle_run_props(target, delta, theme);
    if let Some(t) = delta.bold {
        target.bold = t.apply_in_style(target.bold);
    }
    if let Some(t) = delta.italic {
        target.italic = t.apply_in_style(target.italic);
    }
    if let Some(t) = delta.caps {
        target.caps = t.apply_in_style(target.caps);
    }
    if let Some(t) = delta.small_caps {
        target.small_caps = t.apply_in_style(target.small_caps);
    }
    if let Some(t) = delta.strike {
        target.strike = t.apply_in_style(target.strike);
    }
    if let Some(t) = delta.double_strike {
        target.double_strike = t.apply_in_style(target.double_strike);
        if target.double_strike {
            target.strike = true;
        }
    }
    if let Some(t) = delta.vanish {
        target.vanish = t.apply_in_style(target.vanish);
    }
}

fn apply_run_props_direct(target: &mut ResolvedRunStyle, delta: &RunProperties, theme: &WordTheme) {
    apply_non_toggle_run_props(target, delta, theme);
    if let Some(t) = delta.bold {
        target.bold = t.apply_direct();
    }
    if let Some(t) = delta.italic {
        target.italic = t.apply_direct();
    }
    if let Some(t) = delta.caps {
        target.caps = t.apply_direct();
    }
    if let Some(t) = delta.small_caps {
        target.small_caps = t.apply_direct();
    }
    if let Some(t) = delta.strike {
        target.strike = t.apply_direct();
    }
    if let Some(t) = delta.double_strike {
        target.double_strike = t.apply_direct();
        target.strike = target.double_strike;
    }
    if let Some(t) = delta.vanish {
        target.vanish = t.apply_direct();
    }
}

fn apply_non_toggle_run_props(
    target: &mut ResolvedRunStyle,
    delta: &RunProperties,
    theme: &WordTheme,
) {
    let (latin, east_asia, cs) = resolve_font_slots(&delta.fonts, theme);
    if let Some(f) = latin {
        target.font_family = f;
    }
    if let Some(ea) = east_asia {
        target.east_asia_font = Some(ea);
    }
    if let Some(c) = cs {
        target.cs_font = Some(c);
    }
    if let Some(sz) = delta.font_size_pt.or(delta.font_size_cs_pt) {
        target.font_size_pt = sz;
    }
    if let Some(u) = delta.underline {
        target.underline = u;
    }
    if let Some(ref c) = delta.color {
        target.color.clone_from(c);
    }
    if let Some(ref hl) = delta.highlight_color {
        target.highlight_color = Some(hl.clone());
    }
    if let Some(sp) = delta.char_spacing_pt {
        target.char_spacing_pt = sp;
    }
    if let Some(va) = delta.vert_align {
        target.vert_align = va;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToggleProp, UnderlineKind, WordAlignment};
    use std::collections::BTreeMap;

    #[test]
    fn non_ascii_color_values_are_ignored() {
        let mut theme = WordTheme::default();
        theme.colors.insert(
            "accent1".to_string(),
            "\u{20ac}\u{20ac}\u{20ac}".to_string(),
        );
        assert_eq!(
            resolve_word_color(None, Some("accent1"), Some(0x99), None, &theme),
            None
        );
        assert_eq!(parse_hex_rgb("\u{e9}12345"), None);
    }

    fn make_style(id: &str, style_type: StyleType, based_on: Option<&str>) -> WordStyle {
        WordStyle {
            id: id.to_string(),
            name: Some(id.to_string()),
            style_type,
            is_default: false,
            based_on: based_on.map(str::to_string),
            next: None,
            link: None,
            para_props: ParagraphProperties::default(),
            run_props: RunProperties::default(),
            table_borders: WordBorders::default(),
            table_conditionals: BTreeMap::new(),
        }
    }

    #[test]
    fn toggle_xor_in_style_chain_and_direct_override() {
        let mut sheet = WordStyleSheet::default();

        // Base paragraph style turns bold ON
        let mut base = make_style("Base", StyleType::Paragraph, None);
        base.run_props.bold = Some(ToggleProp::Toggle);
        base.run_props.font_size_pt = Some(14.0);

        // Child paragraph style also has `<w:b/>` (Toggle), which XORs bold to FALSE!
        let mut child = make_style("Child", StyleType::Paragraph, Some("Base"));
        child.run_props.bold = Some(ToggleProp::Toggle);
        child.run_props.italic = Some(ToggleProp::Toggle);

        sheet.styles.insert("Base".to_string(), base);
        sheet.styles.insert("Child".to_string(), child);

        let theme = WordTheme::default();

        // 1. Style Base alone -> bold = true
        let r_base = sheet.resolve_run_style(Some("Base"), None, &RunProperties::default(), &theme);
        assert!(r_base.bold);
        assert_eq!(r_base.font_size_pt, 14.0);

        // 2. Style Child (basedOn Base) -> bold XORed to false, italic = true, size = 14.0 inherited
        let r_child =
            sheet.resolve_run_style(Some("Child"), None, &RunProperties::default(), &theme);
        assert!(
            !r_child.bold,
            "two Toggle levels in style chain must XOR to false"
        );
        assert!(r_child.italic);
        assert_eq!(r_child.font_size_pt, 14.0);

        // 3. Direct formatting `<w:b w:val="0"/>` on Base -> turns bold OFF
        let direct_off = RunProperties {
            bold: Some(ToggleProp::Off),
            ..RunProperties::default()
        };
        let r_off = sheet.resolve_run_style(Some("Base"), None, &direct_off, &theme);
        assert!(
            !r_off.bold,
            "explicit Off in direct formatting must turn bold off"
        );

        // 4. Direct formatting `<w:b/>` on Base -> stays true (direct formatting is override, not XOR)
        let direct_on = RunProperties {
            bold: Some(ToggleProp::Toggle),
            ..RunProperties::default()
        };
        let r_on = sheet.resolve_run_style(Some("Base"), None, &direct_on, &theme);
        assert!(r_on.bold, "direct Toggle must set bold to true");
    }

    #[test]
    fn cyclic_based_on_terminates_cleanly() {
        let mut sheet = WordStyleSheet::default();
        let mut a = make_style("StyleA", StyleType::Paragraph, Some("StyleB"));
        a.para_props.alignment = Some(WordAlignment::Center);
        let mut b = make_style("StyleB", StyleType::Paragraph, Some("StyleA"));
        b.para_props.space_after_pt = Some(24.0);
        sheet.styles.insert("StyleA".to_string(), a);
        sheet.styles.insert("StyleB".to_string(), b);

        let chain = sheet.style_chain(Some("StyleA"), None);
        assert_eq!(chain.len(), 2);

        let resolved =
            sheet.resolve_paragraph_style(Some("StyleA"), None, &ParagraphProperties::default());
        assert_eq!(resolved.alignment, WordAlignment::Center);
        assert_eq!(resolved.space_after_pt, 24.0);
    }

    #[test]
    fn theme_color_tint_and_shade_resolution() {
        let mut theme = WordTheme::default();
        theme
            .colors
            .insert("accent1".to_string(), "4F81BD".to_string());
        theme.colors.insert("dk1".to_string(), "000000".to_string());

        // Base theme color without tint/shade
        assert_eq!(
            resolve_word_color(None, Some("accent1"), None, None, &theme).as_deref(),
            Some("#4F81BD")
        );
        // Word alias "text1" -> "dk1"
        assert_eq!(
            resolve_word_color(None, Some("text1"), None, None, &theme).as_deref(),
            Some("#000000")
        );
        // Tint 0x80 (~50% toward white)
        let tinted = resolve_word_color(None, Some("dk1"), Some(0x80), None, &theme).unwrap();
        assert_eq!(tinted, "#7F7F7F");
        // Shade 0x80 (~50% toward black)
        let shaded = resolve_word_color(None, Some("accent1"), None, Some(0x80), &theme).unwrap();
        assert_eq!(shaded, "#28415F");
    }

    #[test]
    fn underline_and_font_slots_resolve_with_theme() {
        let theme = WordTheme {
            major_latin: Some("Cambria".to_string()),
            minor_latin: Some("Calibri".to_string()),
            ..WordTheme::default()
        };

        let mut sheet = WordStyleSheet::default();
        let mut h1 = make_style("Heading1", StyleType::Paragraph, None);
        h1.run_props.fonts.ascii_theme = Some("majorHAnsi".to_string());
        h1.run_props.underline = Some(UnderlineKind::Single);
        sheet.styles.insert("Heading1".to_string(), h1);

        let r = sheet.resolve_run_style(Some("Heading1"), None, &RunProperties::default(), &theme);
        assert_eq!(r.font_family, "Cambria");
        assert_eq!(r.underline, UnderlineKind::Single);
    }
}
