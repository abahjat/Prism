# ADR 0005: Word Style Inheritance, Toggle XOR Rules, and Theme Resolution

- **Status**: Accepted
- **Date**: 2026-10-23
- **Phase**: Phase 2 (Milestone M0 / M1)

## Context

In WordprocessingML (ECMA-376 Part 1 §17.7.2), formatting properties are not copied when styles are defined; they are dynamically evaluated through a multi-level inheritance hierarchy. Additionally, boolean "toggle" properties (`w:b`, `w:i`, `w:caps`, `w:smallCaps`, `w:strike`, `w:dstrike`, `w:vanish`, `w:emboss`, `w:imprint`, `w:outline`, `w:shadow`) behave differently depending on whether they appear in a style chain or in direct formatting.

## Decision

We implement computed style resolution in `prism_word::inheritance` following ECMA-376 §17.7.2 and §17.7.3:

### 1. Property Evaluation Precedence (Lowest to Highest)
1. **Document Defaults (`w:docDefaults`)**:
   - `w:pPrDefault/w:pPr` (default paragraph properties)
   - `w:rPrDefault/w:rPr` (default run properties; defaults to 10 pt or 11 pt Calibri depending on compat mode / theme)
2. **Table Style (`w:tblStyle`)**:
   - Base table properties + conditional formatting (`w:tblStylePr`) evaluated in ECMA-376 priority order:
     `wholeTable` → `band1Vert` / `band2Vert` → `band1Horz` / `band2Horz` → `firstRow` / `lastRow` → `firstCol` / `lastCol` → corner cells (`nwCell`, `neCell`, `swCell`, `seCell`).
3. **Numbering Properties (`w:numPr`)**:
   - Paragraph and run properties from the resolved `w:abstractNum` level (`w:lvl/w:pPr` and `w:lvl/w:rPr`), including `w:lvlOverride`.
4. **Paragraph Style Chain (`w:pStyle`)**:
   - Walk `basedOn` ancestors from root to leaf (guarded by `MAX_STYLE_DEPTH = 64` and a visited-ID set to prevent cycles).
5. **Character Style Chain (`w:rStyle`)**:
   - Walk `basedOn` ancestors from root to leaf (with the same cycle guard).
6. **Direct Formatting**:
   - Paragraph direct `w:pPr` and run direct `w:rPr`.

### 2. Toggle Properties (`ToggleProp`)
Every toggle property is represented in raw `RunProperties` as `Option<ToggleProp>`:
- `ToggleProp::Toggle` (`<w:b/>` or `<w:b w:val="true"/>` / `<w:b w:val="1"/>`):
  - Within style levels (table style → paragraph style chain → character style chain), `ToggleProp::Toggle` **XORs** the inherited boolean state (`state = !state`), unless the same style hierarchy already set it.
  - In **direct formatting** (`w:r/w:rPr`), `ToggleProp::Toggle` unconditionally sets `state = true`.
- `ToggleProp::Off` (`<w:b w:val="0"/>` or `<w:b w:val="false"/>`):
  - Explicitly sets `state = false` at any level (and in direct formatting overrides any `true` inherited from styles).

### 3. Theme Fonts & Colors
- `w:rFonts` supports `ascii`, `hAnsi`, `eastAsia`, `cs`, and theme slots (`majorHAnsi`, `minorHAnsi`, `majorEastAsia`, `minorEastAsia`, `majorBidi`, `minorBidi`), resolved against `word/theme/theme1.xml`.
- `w:color` and `w:shd` resolve `w:themeColor` against the theme's `clrScheme`, applying `w:themeTint` and `w:themeShade` luminance transforms in HSL space per ECMA-376 §17.3.2.6.

## Consequences

- **Positive**: Matches Word's exact inheritance behavior on nested styles, inverted bold/italic in headings or captions, and explicit `<w:b w:val="0"/>` overrides.
- **Positive**: Cycle detection guarantees termination on malformed or adversarial `styles.xml` files.
