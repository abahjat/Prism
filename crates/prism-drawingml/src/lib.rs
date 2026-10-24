// SPDX-License-Identifier: AGPL-3.0-only
//! Shared `DrawingML` engine (`prism-drawingml`) for `PowerPoint`, Excel, and Word.
//!
//! Provides:
//! - [`theme`]: Office theme color/font schemes and spec-ordered HSL/RGB color transforms.
//! - [`geometry`]: All 187 ECMA-376 preset shape geometries, the `DrawingML` guide formula
//!   evaluator (`avLst` / `gdLst`), and custom geometry (`a:custGeom`) path construction.
//! - [`chart`]: Shared `DrawingML` (`c:chartSpace` and `cx:chartSpace`) chart parser,
//!   Excel automatic axis scaling, and vector chart renderer from cached series data.
//! - [`xml`]: `quick-xml` helpers shared by the Office readers.

#![warn(missing_docs)]
#![warn(clippy::all, clippy::pedantic)]

pub mod chart;
pub mod geometry;
pub mod theme;
pub mod xml;

pub use chart::{
    AxisScale, AxisSpec, ChartKind, ChartSeries, ChartSpace, LegendPosition,
    compute_excel_axis_scale, parse_chart_xml, render_chart,
};
pub use geometry::{
    ALL_PRESET_SHAPES, CustomGeometry, CustomPath, CustomPathCmd, GuideDef,
    build_custom_geometry_paths, build_preset_geometry_paths, eval_guide_formula,
    is_known_preset_shape, preset_geometry_definition,
};
pub use theme::{
    ColorTransform, DrawingTheme, apply_color_transforms, parse_drawing_theme, parse_hex_rgba,
};
