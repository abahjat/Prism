# ADR 0007: Shared DrawingML Crate (`prism-drawingml`) and Presentation Model (`prism-ppt`)

## Status

Accepted (Phase 3, Milestone M0)

## Context

Phase 3 brings native layout and rendering to PowerPoint (`PPTX` and legacy binary `PPT`) and Excel (`XLSX` and legacy binary `XLS`), as well as native DrawingML chart rendering across Word, PowerPoint, and Excel.

DrawingML (`xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"`, `xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"`) is shared across all three Office application families:
- Theme color schemes, font schemes, and HSL/RGB color transforms (`lumMod`, `lumOff`, `tint`, `shade`, `satMod`, `alpha`).
- Preset shape geometries (~187 shapes in ECMA-376 `presetShapeDefinitions.xml`) and custom geometry (`a:custGeom`) evaluated via the DrawingML guide formula language.
- Fills (solid, linear/radial/path gradient, pattern, blip fill) and strokes (width, dash, compound, arrowheads).
- Charts (`c:chartSpace` and Office 2016+ `cx:chartSpace`).

PowerPoint presentations also require a format-independent `Presentation` model (`prism-ppt`) that both the OOXML `.pptx` reader and the legacy `[MS-PPT]` binary reader populate before laying out slides onto `prism-layout`'s `LayoutDocument`.

## Decision

1. **Create `crates/prism-drawingml`**:
   - Houses `DrawingTheme` and spec-ordered color transform evaluation, the 187 ECMA-376 preset geometry definitions and guide formula interpreter (`avLst` + `gdLst` + `pathLst`), shape fills/strokes, and the shared DrawingML chart parser and renderer (`c:chartSpace` + `cx:chartSpace`).
2. **Create `crates/prism-ppt`**:
   - Defines `Presentation`, `SlideMaster`, `SlideLayout`, `Slide`, `SlideShape`, `PlaceholderRef`, `TextBody`, `AutofitMode`, `SlideTable`, `SmartArtDiagram`, `MediaPoster`, `SlideComment`, `NotesSlide`, and `PresentationForensicFacts`.
   - Both `parse_pptx` (OOXML) and `parse_ppt` (legacy `[MS-PPT]` OLE2/Escher) produce `Presentation`.
   - Placeholder inheritance (`master -> layout -> slide`) matches by `(PlaceholderType, Option<u32>)`, inheriting bounds, preset geometry, and list level text styles.
   - Reuses `prism-word::emf::render_metafile` for WMF/EMF vector metafiles embedded in slides.

## Consequences

- Zero duplication of preset geometry, theme color transforms, or chart rendering between Word, PowerPoint, and Excel.
- Both `.pptx` and `.ppt` share a single deterministic slide layout and notes/review-mode pipeline.
