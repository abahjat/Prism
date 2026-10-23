// SPDX-License-Identifier: AGPL-3.0-only
//! Font discovery, bundled metric-compatible fonts, substitution table, and `.odttf` deobfuscation.

use crate::model::{FontId, FontReport, FontSourceKind, FontStore, LayoutOptions};
use fontique::{
    Blob, Collection, CollectionOptions, FontInfoOverride, FontStyle, FontWeight, FontWidth,
    GenericFamily, Script, SourceCache,
};
use parley::FontContext;
use prism_core::document::FontResource;
use prism_core::error::{Error, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

// Bundled SIL OFL-1.1 metric-compatible font faces (`assets/fonts/`)
const CARLITO_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Carlito-Regular.ttf");
const CARLITO_BOLD: &[u8] = include_bytes!("../../../assets/fonts/Carlito-Bold.ttf");
const CARLITO_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Carlito-Italic.ttf");
const CARLITO_BOLD_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Carlito-BoldItalic.ttf");

const CALADEA_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Caladea-Regular.ttf");
const CALADEA_BOLD: &[u8] = include_bytes!("../../../assets/fonts/Caladea-Bold.ttf");
const CALADEA_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Caladea-Italic.ttf");
const CALADEA_BOLD_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Caladea-BoldItalic.ttf");

const LIBERATION_SANS_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-Regular.ttf");
const LIBERATION_SANS_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationSans-Bold.ttf");
const LIBERATION_SANS_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-Italic.ttf");
const LIBERATION_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSans-BoldItalic.ttf");

const LIBERATION_SERIF_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-Regular.ttf");
const LIBERATION_SERIF_BOLD: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-Bold.ttf");
const LIBERATION_SERIF_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-Italic.ttf");
const LIBERATION_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationSerif-BoldItalic.ttf");

const LIBERATION_MONO_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationMono-Regular.ttf");
const LIBERATION_MONO_BOLD: &[u8] = include_bytes!("../../../assets/fonts/LiberationMono-Bold.ttf");
const LIBERATION_MONO_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationMono-Italic.ttf");
const LIBERATION_MONO_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/LiberationMono-BoldItalic.ttf");

const NOTO_SANS_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf");
const NOTO_SANS_BOLD: &[u8] = include_bytes!("../../../assets/fonts/NotoSans-Bold.ttf");
const NOTO_SANS_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/NotoSans-Italic.ttf");
const NOTO_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSans-BoldItalic.ttf");

const NOTO_SERIF_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/NotoSerif-Regular.ttf");
const NOTO_SERIF_BOLD: &[u8] = include_bytes!("../../../assets/fonts/NotoSerif-Bold.ttf");
const NOTO_SERIF_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/NotoSerif-Italic.ttf");
const NOTO_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSerif-BoldItalic.ttf");

struct BundledFaceSpec {
    family: &'static str,
    weight: u16,
    italic: bool,
    bytes: &'static [u8],
}

const BUNDLED_FACES: &[BundledFaceSpec] = &[
    // Carlito (metric-compatible with Calibri)
    BundledFaceSpec {
        family: "Carlito",
        weight: 400,
        italic: false,
        bytes: CARLITO_REGULAR,
    },
    BundledFaceSpec {
        family: "Carlito",
        weight: 700,
        italic: false,
        bytes: CARLITO_BOLD,
    },
    BundledFaceSpec {
        family: "Carlito",
        weight: 400,
        italic: true,
        bytes: CARLITO_ITALIC,
    },
    BundledFaceSpec {
        family: "Carlito",
        weight: 700,
        italic: true,
        bytes: CARLITO_BOLD_ITALIC,
    },
    // Caladea (metric-compatible with Cambria)
    BundledFaceSpec {
        family: "Caladea",
        weight: 400,
        italic: false,
        bytes: CALADEA_REGULAR,
    },
    BundledFaceSpec {
        family: "Caladea",
        weight: 700,
        italic: false,
        bytes: CALADEA_BOLD,
    },
    BundledFaceSpec {
        family: "Caladea",
        weight: 400,
        italic: true,
        bytes: CALADEA_ITALIC,
    },
    BundledFaceSpec {
        family: "Caladea",
        weight: 700,
        italic: true,
        bytes: CALADEA_BOLD_ITALIC,
    },
    // Liberation Sans (metric-compatible with Arial / Helvetica)
    BundledFaceSpec {
        family: "Liberation Sans",
        weight: 400,
        italic: false,
        bytes: LIBERATION_SANS_REGULAR,
    },
    BundledFaceSpec {
        family: "Liberation Sans",
        weight: 700,
        italic: false,
        bytes: LIBERATION_SANS_BOLD,
    },
    BundledFaceSpec {
        family: "Liberation Sans",
        weight: 400,
        italic: true,
        bytes: LIBERATION_SANS_ITALIC,
    },
    BundledFaceSpec {
        family: "Liberation Sans",
        weight: 700,
        italic: true,
        bytes: LIBERATION_SANS_BOLD_ITALIC,
    },
    // Liberation Serif (metric-compatible with Times New Roman)
    BundledFaceSpec {
        family: "Liberation Serif",
        weight: 400,
        italic: false,
        bytes: LIBERATION_SERIF_REGULAR,
    },
    BundledFaceSpec {
        family: "Liberation Serif",
        weight: 700,
        italic: false,
        bytes: LIBERATION_SERIF_BOLD,
    },
    BundledFaceSpec {
        family: "Liberation Serif",
        weight: 400,
        italic: true,
        bytes: LIBERATION_SERIF_ITALIC,
    },
    BundledFaceSpec {
        family: "Liberation Serif",
        weight: 700,
        italic: true,
        bytes: LIBERATION_SERIF_BOLD_ITALIC,
    },
    // Liberation Mono (metric-compatible with Courier New)
    BundledFaceSpec {
        family: "Liberation Mono",
        weight: 400,
        italic: false,
        bytes: LIBERATION_MONO_REGULAR,
    },
    BundledFaceSpec {
        family: "Liberation Mono",
        weight: 700,
        italic: false,
        bytes: LIBERATION_MONO_BOLD,
    },
    BundledFaceSpec {
        family: "Liberation Mono",
        weight: 400,
        italic: true,
        bytes: LIBERATION_MONO_ITALIC,
    },
    BundledFaceSpec {
        family: "Liberation Mono",
        weight: 700,
        italic: true,
        bytes: LIBERATION_MONO_BOLD_ITALIC,
    },
    // Noto Sans
    BundledFaceSpec {
        family: "Noto Sans",
        weight: 400,
        italic: false,
        bytes: NOTO_SANS_REGULAR,
    },
    BundledFaceSpec {
        family: "Noto Sans",
        weight: 700,
        italic: false,
        bytes: NOTO_SANS_BOLD,
    },
    BundledFaceSpec {
        family: "Noto Sans",
        weight: 400,
        italic: true,
        bytes: NOTO_SANS_ITALIC,
    },
    BundledFaceSpec {
        family: "Noto Sans",
        weight: 700,
        italic: true,
        bytes: NOTO_SANS_BOLD_ITALIC,
    },
    // Noto Serif
    BundledFaceSpec {
        family: "Noto Serif",
        weight: 400,
        italic: false,
        bytes: NOTO_SERIF_REGULAR,
    },
    BundledFaceSpec {
        family: "Noto Serif",
        weight: 700,
        italic: false,
        bytes: NOTO_SERIF_BOLD,
    },
    BundledFaceSpec {
        family: "Noto Serif",
        weight: 400,
        italic: true,
        bytes: NOTO_SERIF_ITALIC,
    },
    BundledFaceSpec {
        family: "Noto Serif",
        weight: 700,
        italic: true,
        bytes: NOTO_SERIF_BOLD_ITALIC,
    },
];

/// Data-driven Office & PDF font substitution table `(normalized_requested_name, bundled_family)`.
const SUBSTITUTION_TABLE: &[(&str, &str)] = &[
    // Carlito (Calibri metric-compatible)
    ("calibri", "Carlito"),
    ("calibri light", "Carlito"),
    ("calibrilight", "Carlito"),
    ("carlito", "Carlito"),
    // Caladea (Cambria metric-compatible)
    ("cambria", "Caladea"),
    ("cambria math", "Caladea"),
    ("cambriamath", "Caladea"),
    ("caladea", "Caladea"),
    // Liberation Sans (Arial / Helvetica metric-compatible)
    ("arial", "Liberation Sans"),
    ("arial narrow", "Liberation Sans"),
    ("arial black", "Liberation Sans"),
    ("arial unicode ms", "Liberation Sans"),
    ("arialmt", "Liberation Sans"),
    ("helvetica", "Liberation Sans"),
    ("helvetica neue", "Liberation Sans"),
    ("helveticaneue", "Liberation Sans"),
    ("liberation sans", "Liberation Sans"),
    ("liberationsans", "Liberation Sans"),
    ("arimo", "Liberation Sans"),
    ("swiss", "Liberation Sans"),
    ("sans-serif", "Liberation Sans"),
    ("sans", "Liberation Sans"),
    // Liberation Serif (Times New Roman metric-compatible)
    ("times new roman", "Liberation Serif"),
    ("timesnewroman", "Liberation Serif"),
    ("timesnewromanps", "Liberation Serif"),
    ("timesnewromanpsmt", "Liberation Serif"),
    ("times", "Liberation Serif"),
    ("times-roman", "Liberation Serif"),
    ("timesroman", "Liberation Serif"),
    ("liberation serif", "Liberation Serif"),
    ("liberationserif", "Liberation Serif"),
    ("tinos", "Liberation Serif"),
    ("serif", "Liberation Serif"),
    // Liberation Mono (Courier New metric-compatible)
    ("courier new", "Liberation Mono"),
    ("couriernew", "Liberation Mono"),
    ("couriernewps", "Liberation Mono"),
    ("couriernewpsmt", "Liberation Mono"),
    ("courier", "Liberation Mono"),
    ("consolas", "Liberation Mono"),
    ("lucida console", "Liberation Mono"),
    ("lucidaconsole", "Liberation Mono"),
    ("monaco", "Liberation Mono"),
    ("menlo", "Liberation Mono"),
    ("liberation mono", "Liberation Mono"),
    ("liberationmono", "Liberation Mono"),
    ("cousine", "Liberation Mono"),
    ("monospace", "Liberation Mono"),
    ("mono", "Liberation Mono"),
    // Noto Sans (general sans-serif & modern Office defaults)
    ("aptos", "Noto Sans"),
    ("aptos display", "Noto Sans"),
    ("aptos narrow", "Noto Sans"),
    ("segoe ui", "Noto Sans"),
    ("segoe ui light", "Noto Sans"),
    ("segoe ui semibold", "Noto Sans"),
    ("tahoma", "Noto Sans"),
    ("verdana", "Noto Sans"),
    ("trebuchet ms", "Noto Sans"),
    ("candara", "Noto Sans"),
    ("corbel", "Noto Sans"),
    ("franklin gothic medium", "Noto Sans"),
    ("gill sans", "Noto Sans"),
    ("optima", "Noto Sans"),
    ("futura", "Noto Sans"),
    ("system-ui", "Noto Sans"),
    ("noto sans", "Noto Sans"),
    ("notosans", "Noto Sans"),
    // Noto Serif (general serif)
    ("georgia", "Noto Serif"),
    ("palatino", "Noto Serif"),
    ("palatino linotype", "Noto Serif"),
    ("book antiqua", "Noto Serif"),
    ("garamond", "Noto Serif"),
    ("constantia", "Noto Serif"),
    ("baskerville", "Noto Serif"),
    ("bookman old style", "Noto Serif"),
    ("century", "Noto Serif"),
    ("century schoolbook", "Noto Serif"),
    ("sylfaen", "Noto Serif"),
    ("noto serif", "Noto Serif"),
    ("notoserif", "Noto Serif"),
];

/// Strip a 6-character uppercase PDF subset prefix (`ABCDEF+FontName` → `FontName`).
#[must_use]
pub fn strip_subset_prefix(name: &str) -> &str {
    if let Some((prefix, rest)) = name.split_once('+')
        && prefix.len() == 6
        && prefix.chars().all(|c| c.is_ascii_uppercase())
    {
        return rest;
    }
    name
}

/// Normalize a font family name by stripping PDF subset prefixes and PostScript style suffixes.
#[must_use]
pub fn normalize_family_key(requested: &str) -> String {
    let trimmed = requested.trim().trim_matches(['"', '\'']);
    let without_subset = strip_subset_prefix(trimmed);
    let before_comma = without_subset
        .split_once(',')
        .map_or(without_subset, |(base, _)| base);

    let lower = before_comma.trim().to_ascii_lowercase();
    let stripped = [
        "-bolditalicmt",
        "-bolditalic",
        "-boldoblique",
        "-boldmt",
        "-bold",
        "-italicmt",
        "-italic",
        "-oblique",
        "-regular",
        "-roman",
        "psmt",
    ]
    .iter()
    .find_map(|suffix| lower.strip_suffix(suffix))
    .unwrap_or(&lower);

    stripped.trim().to_string()
}

/// Resolve a requested font family name against the data-driven substitution table.
///
/// Returns `(resolved_bundled_family, matched_in_table)`. If the font is not in the table,
/// falls back to `"Noto Sans"` with `matched_in_table = false`.
#[must_use]
pub fn substitute_font_family(requested: &str) -> (&'static str, bool) {
    let key = normalize_family_key(requested);
    for &(candidate, target) in SUBSTITUTION_TABLE {
        if key == candidate {
            return (target, true);
        }
    }
    ("Noto Sans", false)
}

/// Deobfuscate an OOXML / XPS `.odttf` embedded font stream using its GUID key
/// (ECMA-376 Part 1 §17.8.1).
///
/// The 16-byte GUID is parsed from big-endian hex digits, reversed in byte order,
/// and `XORed` with the first 32 bytes of `data`.
///
/// # Errors
///
/// Returns [`Error::ParseError`] if `guid` does not contain 32 hexadecimal digits or
/// if `data` is shorter than 32 bytes.
pub fn deobfuscate_odttf(data: &[u8], guid: &str) -> Result<Vec<u8>> {
    if data.len() < 32 {
        return Err(Error::ParseError(format!(
            "ODTTF font stream is too short ({} bytes; need at least 32)",
            data.len()
        )));
    }
    let hex_digits: String = guid.chars().filter(char::is_ascii_hexdigit).collect();
    if hex_digits.len() != 32 {
        return Err(Error::ParseError(format!(
            "Invalid ODTTF font GUID {guid:?}: expected 32 hex digits, got {}",
            hex_digits.len()
        )));
    }

    let mut guid_bytes = [0u8; 16];
    for i in 0..16 {
        let byte_str = &hex_digits[i * 2..i * 2 + 2];
        guid_bytes[i] = u8::from_str_radix(byte_str, 16)
            .map_err(|e| Error::ParseError(format!("Invalid hex in ODTTF GUID: {e}")))?;
    }

    let mut out = data.to_vec();
    for i in 0..32 {
        let key_byte = guid_bytes[15 - (i % 16)];
        out[i] ^= key_byte;
    }
    Ok(out)
}

/// Hermetic font manager that coordinates `parley::FontContext`, `FontStore`, and `FontReport`.
pub struct FontSystem {
    /// Parley font context backed by an isolated `fontique::Collection`.
    pub font_cx: FontContext,
    /// Positioned page model font store.
    pub store: FontStore,
    /// Per-document font substitution report.
    pub report: FontReport,
    /// Lookup from `(family_lowercase, weight, italic)` to `FontId`.
    face_map: BTreeMap<(String, u16, bool), FontId>,
    /// Lookup from `Blob::id()` (`u64`) + face index (`u32`) to `FontId`.
    blob_map: BTreeMap<(u64, u32), FontId>,
    /// Set of embedded family names (lowercase) registered from the document.
    embedded_families: BTreeMap<String, String>,
}

impl Default for FontSystem {
    fn default() -> Self {
        Self::new(&LayoutOptions::default())
    }
}

impl FontSystem {
    /// Construct a new [`FontSystem`] populated with Prism's bundled OFL-1.1 fonts.
    ///
    /// System fonts are never loaded unless `options.allow_system_fonts` is `true`.
    #[must_use]
    pub fn new(options: &LayoutOptions) -> Self {
        let collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let mut sys = Self {
            font_cx: FontContext {
                collection,
                source_cache: SourceCache::default(),
            },
            store: FontStore::new(),
            report: FontReport::new(),
            face_map: BTreeMap::new(),
            blob_map: BTreeMap::new(),
            embedded_families: BTreeMap::new(),
        };
        sys.register_bundled_fonts();
        if options.allow_system_fonts {
            sys.register_system_fonts_pure_rust();
        }
        sys
    }

    fn register_bundled_fonts(&mut self) {
        for spec in BUNDLED_FACES {
            self.register_face_bytes(
                spec.family,
                spec.weight,
                spec.italic,
                FontSourceKind::Bundled,
                spec.bytes.to_vec(),
                0,
            );
        }

        // Configure generic family mappings and Unicode script fallbacks in fontique
        if let Some(sans_id) = self.font_cx.collection.family_id("Liberation Sans") {
            self.font_cx
                .collection
                .set_generic_families(GenericFamily::SansSerif, std::iter::once(sans_id));
            self.font_cx
                .collection
                .set_generic_families(GenericFamily::SystemUi, std::iter::once(sans_id));
        }
        if let Some(serif_id) = self.font_cx.collection.family_id("Liberation Serif") {
            self.font_cx
                .collection
                .set_generic_families(GenericFamily::Serif, std::iter::once(serif_id));
        }
        if let Some(mono_id) = self.font_cx.collection.family_id("Liberation Mono") {
            self.font_cx
                .collection
                .set_generic_families(GenericFamily::Monospace, std::iter::once(mono_id));
        }
        if let Some(noto_sans_id) = self.font_cx.collection.family_id("Noto Sans") {
            for script in [
                Script::from_bytes(*b"Latn"),
                Script::from_bytes(*b"Cyrl"),
                Script::from_bytes(*b"Grek"),
            ] {
                self.font_cx
                    .collection
                    .append_fallbacks(script, std::iter::once(noto_sans_id));
            }
        }
    }

    /// Register a TrueType/OpenType font face into both `fontique::Collection` and `FontStore`.
    pub fn register_face_bytes(
        &mut self,
        family: &str,
        weight: u16,
        italic: bool,
        source: FontSourceKind,
        bytes: Vec<u8>,
        face_index: u32,
    ) -> FontId {
        let blob = Blob::new(Arc::new(bytes.clone()));
        let blob_id = blob.id();

        let info_override = FontInfoOverride {
            family_name: Some(family),
            width: Some(FontWidth::NORMAL),
            style: Some(if italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            }),
            weight: Some(FontWeight::new(f32::from(weight))),
            axes: None,
        };
        let _ = self
            .font_cx
            .collection
            .register_fonts(blob, Some(info_override));

        let font_id = self.store.register_ttf(
            family.to_string(),
            weight,
            italic,
            source,
            bytes,
            face_index,
        );
        self.face_map
            .insert((family.to_ascii_lowercase(), weight, italic), font_id);
        self.blob_map.insert((blob_id, face_index), font_id);

        if source == FontSourceKind::Embedded {
            self.embedded_families
                .insert(family.to_ascii_lowercase(), family.to_string());
        }
        font_id
    }

    /// Register embedded [`FontResource`] entries from a UDM document.
    pub fn register_document_fonts(&mut self, resources: &[FontResource]) {
        for res in resources {
            let Some(ref raw_data) = res.data else {
                continue;
            };
            if raw_data.len() < 12 {
                continue;
            }
            let style_lower = res.style.to_ascii_lowercase();
            let weight = if style_lower.contains("bold") {
                700
            } else {
                400
            };
            let italic = style_lower.contains("italic") || style_lower.contains("oblique");
            self.register_face_bytes(
                &res.family,
                weight,
                italic,
                FontSourceKind::Embedded,
                raw_data.clone(),
                0,
            );
        }
    }

    /// Resolve a requested font family name, recording the substitution in [`Self::report`],
    /// and return the family name to pass to `parley`.
    pub fn resolve_family(&mut self, requested: &str) -> String {
        let trimmed = requested.trim();
        let req_key = normalize_family_key(trimmed);

        // 1. Check document-embedded fonts first
        if let Some(embedded_name) = self.embedded_families.get(&req_key).cloned() {
            self.report
                .record(trimmed, embedded_name.clone(), FontSourceKind::Embedded);
            return embedded_name;
        }

        // 2. Check if already registered in fontique (e.g. exact bundled name or opt-in system font)
        if let Some(info) = self.font_cx.collection.family_id(&req_key).and_then(|id| {
            self.font_cx
                .collection
                .family(id)
                .map(|f| f.name().to_string())
        }) {
            let source = if BUNDLED_FACES
                .iter()
                .any(|f| f.family.eq_ignore_ascii_case(&info))
            {
                FontSourceKind::Bundled
            } else {
                FontSourceKind::System
            };
            self.report.record(trimmed, info.clone(), source);
            return info;
        }

        // 3. Consult the deterministic Office / PDF substitution table
        let (substituted, _) = substitute_font_family(trimmed);
        self.report
            .record(trimmed, substituted, FontSourceKind::Bundled);
        substituted.to_string()
    }

    /// Look up the [`FontId`] in [`Self::store`] corresponding to a shaped `parley::FontData`.
    #[must_use]
    pub fn font_id_for_parley_font(
        &self,
        font_data: &parley::FontData,
        family_hint: &str,
        bold: bool,
        italic: bool,
    ) -> FontId {
        let blob_id = font_data.data.id();
        if let Some(&id) = self.blob_map.get(&(blob_id, font_data.index)) {
            return id;
        }
        let weight = if bold { 700 } else { 400 };
        if let Some(&id) = self
            .face_map
            .get(&(family_hint.to_ascii_lowercase(), weight, italic))
        {
            return id;
        }
        if let Some(&id) = self
            .face_map
            .get(&(family_hint.to_ascii_lowercase(), 400, false))
        {
            return id;
        }
        FontId(0)
    }

    /// Look up a [`FontId`] directly by family, bold, and italic flags (resolving through the substitution table).
    pub fn resolve_font_id(&mut self, requested_family: &str, bold: bool, italic: bool) -> FontId {
        let resolved = self.resolve_family(requested_family);
        let weight = if bold { 700 } else { 400 };
        let key = (resolved.to_ascii_lowercase(), weight, italic);
        if let Some(&id) = self.face_map.get(&key) {
            return id;
        }
        let fallback_key = (resolved.to_ascii_lowercase(), 400, false);
        self.face_map
            .get(&fallback_key)
            .copied()
            .unwrap_or(FontId(0))
    }

    fn register_system_fonts_pure_rust(&mut self) {
        let candidate_dirs: &[&str] = &[
            #[cfg(target_os = "windows")]
            r"C:\Windows\Fonts",
            #[cfg(target_os = "macos")]
            "/System/Library/Fonts",
            #[cfg(target_os = "macos")]
            "/Library/Fonts",
            #[cfg(target_os = "linux")]
            "/usr/share/fonts",
            #[cfg(target_os = "linux")]
            "/usr/local/share/fonts",
        ];

        for dir in candidate_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("ttf") || ext.eq_ignore_ascii_case("otf")
                        })
                })
                .collect();
            paths.sort();

            for path in paths.into_iter().take(256) {
                if let Ok(bytes) = std::fs::read(&path) {
                    let _ = self
                        .font_cx
                        .collection
                        .register_fonts(Blob::new(Arc::new(bytes)), None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_office_font_substitution_table() {
        assert_eq!(substitute_font_family("Calibri"), ("Carlito", true));
        assert_eq!(substitute_font_family("Calibri Light"), ("Carlito", true));
        assert_eq!(substitute_font_family("Cambria"), ("Caladea", true));
        assert_eq!(substitute_font_family("Arial"), ("Liberation Sans", true));
        assert_eq!(
            substitute_font_family("ABCDEF+Arial-BoldMT"),
            ("Liberation Sans", true)
        );
        assert_eq!(
            substitute_font_family("Times New Roman"),
            ("Liberation Serif", true)
        );
        assert_eq!(
            substitute_font_family("Courier New"),
            ("Liberation Mono", true)
        );
        assert_eq!(substitute_font_family("Segoe UI"), ("Noto Sans", true));
        assert_eq!(substitute_font_family("Georgia"), ("Noto Serif", true));
        assert_eq!(
            substitute_font_family("UnknownCustomFont"),
            ("Noto Sans", false)
        );
    }

    #[test]
    fn test_font_system_records_substitution_report() {
        let mut sys = FontSystem::new(&LayoutOptions::default());
        let resolved1 = sys.resolve_family("Calibri");
        assert_eq!(resolved1, "Carlito");

        let resolved2 = sys.resolve_family("Times New Roman");
        assert_eq!(resolved2, "Liberation Serif");

        let resolved3 = sys.resolve_family("Carlito");
        assert_eq!(resolved3, "Carlito");

        let calibri_entry = sys.report.entries.get("Calibri").expect("Calibri entry");
        assert_eq!(calibri_entry.resolved_family, "Carlito");
        assert_eq!(calibri_entry.source, FontSourceKind::Bundled);
        assert!(!calibri_entry.exact_match);

        let carlito_entry = sys.report.entries.get("Carlito").expect("Carlito entry");
        assert!(carlito_entry.exact_match);
    }

    #[test]
    fn test_odttf_deobfuscation_roundtrip() {
        let guid = "{10111213-1415-1617-1819-1A1B1C1D1E1F}";
        let mut original = vec![0u8; 64];
        // TrueType sfnt version 0x00010000
        original[0..4].copy_from_slice(&[0x00, 0x01, 0x00, 0x00]);
        for (i, b) in original[4..].iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            {
                *b = (i as u8).wrapping_mul(7);
            }
        }

        let obfuscated = deobfuscate_odttf(&original, guid).expect("obfuscate");
        assert_ne!(&obfuscated[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&obfuscated[32..], &original[32..]);

        let restored = deobfuscate_odttf(&obfuscated, guid).expect("deobfuscate");
        assert_eq!(restored, original);
        assert_eq!(&restored[0..4], &[0x00, 0x01, 0x00, 0x00]);
    }
}
