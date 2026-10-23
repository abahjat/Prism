// SPDX-License-Identifier: AGPL-3.0-only
//! Shared `DrawingML` (`c:chartSpace` and `cx:chartSpace`) chart parser, Excel automatic
//! axis scaling, and vector chart renderer from cached series data.

use crate::theme::{DrawingTheme, attr_val, local_name, parse_hex_rgba};
use prism_core::error::{Error, Result};
use prism_layout::engine::{LineSpacing, ParagraphSpec, SpanSpec, TextAlign, TextLayoutEngine};
use prism_layout::fonts::FontSystem;
use prism_layout::model::{
    Affine, FillPaint, FillRule, Item, LineCap, LineJoin, PathCommand, PathItem, Point, Rect, Rgba,
    StrokeStyle,
};
use prism_numfmt::{CellInputValue, FormatOptions, format_with_code};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;

/// Maximum cached points per series to guard against resource exhaustion.
pub const MAX_CHART_POINTS: usize = 10_000;

/// Supported classic (`c:chartSpace`) and Office 2016+ (`cx:chartSpace`) chart types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChartKind {
    /// Clustered vertical column chart (`c:barChart` with `barDir="col"`).
    #[default]
    ColumnClustered,
    /// Stacked vertical column chart (`grouping="stacked"`).
    ColumnStacked,
    /// 100% stacked vertical column chart (`grouping="percentStacked"`).
    ColumnPercentStacked,
    /// Clustered horizontal bar chart (`c:barChart` with `barDir="bar"`).
    BarClustered,
    /// Stacked horizontal bar chart.
    BarStacked,
    /// 100% stacked horizontal bar chart.
    BarPercentStacked,
    /// Line chart (`c:lineChart`, `c:line3DChart`).
    Line,
    /// Area chart (`c:areaChart`, `c:area3DChart`).
    Area,
    /// Pie chart (`c:pieChart`, `c:pie3DChart`, `c:ofPieChart`).
    Pie,
    /// Doughnut chart (`c:doughnutChart`).
    Doughnut,
    /// XY scatter chart (`c:scatterChart`).
    Scatter,
    /// Bubble chart (`c:bubbleChart`).
    Bubble,
    /// Radar chart (`c:radarChart`).
    Radar,
    /// Stock chart (`c:stockChart`).
    Stock,
    /// Office 2016+ Waterfall chart (`cx:series layoutId="waterfall"`).
    Waterfall,
    /// Office 2016+ Funnel chart (`cx:series layoutId="funnel"`).
    Funnel,
    /// Office 2016+ Treemap chart (`cx:series layoutId="treemap"`).
    Treemap,
    /// Office 2016+ Sunburst chart (`cx:series layoutId="sunburst"`).
    Sunburst,
    /// Office 2016+ Histogram chart (`cx:series layoutId="histogram"` | `"paretoLine"`).
    Histogram,
    /// Office 2016+ Box-and-Whisker chart (`cx:series layoutId="boxWhisker"`).
    BoxWhisker,
}

/// Legend placement (`c:legendPos`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LegendPosition {
    /// No legend.
    None,
    /// Right of plot area (`r`).
    #[default]
    Right,
    /// Bottom of chart (`b`).
    Bottom,
    /// Top of chart (`t`).
    Top,
    /// Left of plot area (`l`).
    Left,
    /// Top-right corner (`tr`).
    TopRight,
}

/// A single data series with cached categories and values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartSeries {
    /// Series display name (`c:tx`).
    pub name: Option<String>,
    /// Series chart type (allows combo charts where series have distinct types).
    pub kind: ChartKind,
    /// Cached category labels (`c:cat/c:strRef/c:strCache` or `c:numCache`).
    pub categories: Vec<String>,
    /// Cached numeric Y / value points (`c:val/c:numRef/c:numCache`).
    pub values: Vec<f64>,
    /// Cached numeric X points for `Scatter` / `Bubble` (`c:xVal`).
    pub x_values: Vec<f64>,
    /// Cached bubble sizes (`c:bubbleSize`).
    pub bubble_sizes: Vec<f64>,
    /// Value number format code from `c:numCache/c:formatCode`.
    pub format_code: Option<String>,
    /// Explicit fill/stroke color (`a:srgbClr`).
    pub color: Option<Rgba>,
    /// Whether this series is plotted on the secondary value axis.
    pub use_secondary_axis: bool,
    /// Whether data labels show values (`c:dLbls/c:showVal`).
    pub show_data_labels: bool,
    /// Whether a linear trendline is enabled (`c:trendline`).
    pub has_linear_trendline: bool,
    /// Pie/doughnut slice explosion percentage (`c:explosion`).
    pub explosion_pct: u32,
}

/// Parsed axis specification (`c:valAx`, `c:catAx`, `c:dateAx`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AxisSpec {
    /// Axis title text, if any.
    pub title: Option<String>,
    /// Explicit minimum (`c:scaling/c:min`).
    pub min: Option<f64>,
    /// Explicit maximum (`c:scaling/c:max`).
    pub max: Option<f64>,
    /// Explicit major tick unit (`c:majorUnit`).
    pub major_unit: Option<f64>,
    /// Log base (`c:scaling/c:logBase`), if logarithmic.
    pub log_base: Option<f64>,
    /// Reversed orientation (`c:scaling/c:orientation val="maxMin"`).
    pub reversed: bool,
    /// Number format code (`c:numFmt/@formatCode`).
    pub format_code: Option<String>,
    /// Whether major gridlines are present (`c:majorGridlines`).
    pub show_major_gridlines: bool,
}

/// Computed axis bounds and major step produced by [`compute_excel_axis_scale`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AxisScale {
    /// Axis minimum value.
    pub min: f64,
    /// Axis maximum value.
    pub max: f64,
    /// Major tick interval (`> 0.0`).
    pub major_unit: f64,
}

/// Parsed `DrawingML` chart (`c:chartSpace` or `cx:chartSpace`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartSpace {
    /// Chart title text (`c:title`).
    pub title: Option<String>,
    /// Primary chart type.
    pub primary_kind: ChartKind,
    /// Legend placement.
    pub legend: LegendPosition,
    /// Series list.
    pub series: Vec<ChartSeries>,
    /// Primary value axis specification.
    pub value_axis: AxisSpec,
    /// Category axis specification.
    pub category_axis: AxisSpec,
    /// Doughnut hole size percentage (`10..=90`, default `50`).
    pub hole_size_pct: u32,
    /// Whether any external workbook reference was encountered in `c:externalData`.
    pub external_data_ref: Option<String>,
}

impl ChartSpace {
    /// Returns `true` if at least one series has cached numeric values.
    #[must_use]
    pub fn has_cached_values(&self) -> bool {
        self.series.iter().any(|s| !s.values.is_empty())
    }
}

/// Compute Excel's automatic value-axis `min`, `max`, and `major_unit`.
///
/// Follows Excel's nice-number algorithm:
/// 1. For 100%-stacked charts, fixes range to `0.0..=1.0` (`0%..=100%`) with step `0.2` (`20%`).
/// 2. When `anchor_zero` is true (column/bar/area charts) and all data values are `>= 0`,
///    anchors `min` at `0.0` (and vice-versa when all values are `<= 0`).
/// 3. Adds ~5% headroom on the extreme end before snapping to a nice `1, 2, 5 × 10^k` step
///    targeting 5–9 major intervals.
#[must_use]
pub fn compute_excel_axis_scale(
    values: &[f64],
    explicit_min: Option<f64>,
    explicit_max: Option<f64>,
    explicit_major: Option<f64>,
    anchor_zero: bool,
    is_stacked_100: bool,
) -> AxisScale {
    // `NaN` / infinite bounds parse as `f64` but are not usable axis settings.
    let explicit_min = explicit_min.filter(|v| v.is_finite());
    let explicit_max = explicit_max.filter(|v| v.is_finite());
    let explicit_major = explicit_major.filter(|v| v.is_finite() && *v > 0.0);
    let scale = compute_axis_scale_unchecked(
        values,
        explicit_min,
        explicit_max,
        explicit_major,
        anchor_zero,
        is_stacked_100,
    );
    // Values near `f64::MAX` can still overflow the padding arithmetic; renderers rely on a
    // finite `min < max` (`f64::clamp` panics otherwise).
    if scale.min.is_finite()
        && scale.max.is_finite()
        && scale.max > scale.min
        && scale.major_unit.is_finite()
        && scale.major_unit > 0.0
    {
        scale
    } else {
        AxisScale {
            min: 0.0,
            max: 10.0,
            major_unit: 2.0,
        }
    }
}

fn compute_axis_scale_unchecked(
    values: &[f64],
    explicit_min: Option<f64>,
    explicit_max: Option<f64>,
    explicit_major: Option<f64>,
    anchor_zero: bool,
    is_stacked_100: bool,
) -> AxisScale {
    if is_stacked_100 {
        return AxisScale {
            min: explicit_min.unwrap_or(0.0),
            max: explicit_max.unwrap_or(1.0),
            major_unit: explicit_major.unwrap_or(0.2),
        };
    }

    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return AxisScale {
            min: explicit_min.unwrap_or(0.0),
            max: explicit_max.unwrap_or(10.0),
            major_unit: explicit_major.unwrap_or(2.0),
        };
    }

    let data_min = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let data_max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    let mut raw_min = if anchor_zero && data_min >= 0.0 {
        0.0
    } else {
        data_min
    };
    let mut raw_max = if anchor_zero && data_max <= 0.0 {
        0.0
    } else {
        data_max
    };

    if (raw_max - raw_min).abs() < 1e-9 {
        if raw_max.abs() < 1e-9 {
            raw_min = 0.0;
            raw_max = 10.0;
        } else if raw_max > 0.0 {
            raw_min = 0.0;
            raw_max *= 1.2;
        } else {
            raw_max = 0.0;
            raw_min *= 1.2;
        }
    } else {
        let span = raw_max - raw_min;
        if raw_max > 0.0 && explicit_max.is_none() {
            raw_max += span * 0.05;
        }
        if raw_min < 0.0 && explicit_min.is_none() {
            raw_min -= span * 0.05;
        }
    }

    let eff_min_seed = explicit_min.unwrap_or(raw_min);
    let eff_max_seed = explicit_max.unwrap_or(raw_max);
    let span = (eff_max_seed - eff_min_seed).abs().max(1e-6);

    let major = explicit_major
        .filter(|&u| u > 0.0)
        .unwrap_or_else(|| nice_step(span / 6.0));

    let min = explicit_min.unwrap_or_else(|| (eff_min_seed / major).floor() * major);
    let mut max = explicit_max.unwrap_or_else(|| (eff_max_seed / major).ceil() * major);
    if max <= min {
        max = min + major;
    }

    AxisScale {
        min,
        max,
        major_unit: major,
    }
}

fn nice_step(rough: f64) -> f64 {
    if rough <= 0.0 || !rough.is_finite() {
        return 1.0;
    }
    let exp = rough.log10().floor();
    let base = 10_f64.powf(exp);
    let frac = rough / base;
    let mult = if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    mult * base
}

/// Parse a `DrawingML` `c:chartSpace` or `cx:chartSpace` XML part into a [`ChartSpace`].
///
/// # Errors
///
/// Returns an [`Error`] if the XML is malformed.
#[allow(clippy::too_many_lines, clippy::similar_names)]
pub fn parse_chart_xml(xml: &str) -> Result<ChartSpace> {
    let mut chart = ChartSpace {
        legend: LegendPosition::None,
        hole_size_pct: 50,
        ..ChartSpace::default()
    };
    chart.value_axis.show_major_gridlines = true;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut in_title = false;
    let mut title_text = String::new();
    let mut current_kind = ChartKind::ColumnClustered;
    let mut bar_dir_is_bar = false;
    let mut bar_grouping = String::new();

    let mut current_series: Option<ChartSeries> = None;
    let mut in_ser_tx = false;
    let mut in_cat = false;
    let mut in_val = false;
    let mut in_xval = false;
    let mut in_bubble = false;
    let mut in_pt_v = false;
    let mut pt_idx: Option<usize> = None;
    let mut title_run_ended = false;
    let mut in_format_code = false;
    let mut in_val_ax = false;
    let mut in_dlbls = false;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map(crate::xml::resolve_general_ref)
        {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                let qname = e.name();
                let local = local_name(qname.as_ref());
                match local {
                    b"title" if current_series.is_none() => {
                        in_title = true;
                    }
                    b"barChart" | b"bar3DChart" => {
                        bar_dir_is_bar = false;
                        bar_grouping.clear();
                        current_kind = ChartKind::ColumnClustered;
                    }
                    b"barDir" => {
                        if let Some(val) = attr_val(e, b"val") {
                            bar_dir_is_bar = val == "bar";
                            current_kind = resolve_bar_kind(bar_dir_is_bar, &bar_grouping);
                            if chart.series.is_empty() {
                                chart.primary_kind = current_kind;
                            }
                        }
                    }
                    b"grouping" | b"barGrouping" => {
                        if let Some(val) = attr_val(e, b"val") {
                            bar_grouping = val;
                            if matches!(
                                current_kind,
                                ChartKind::ColumnClustered
                                    | ChartKind::ColumnStacked
                                    | ChartKind::ColumnPercentStacked
                                    | ChartKind::BarClustered
                                    | ChartKind::BarStacked
                                    | ChartKind::BarPercentStacked
                            ) {
                                current_kind = resolve_bar_kind(bar_dir_is_bar, &bar_grouping);
                                if chart.series.is_empty() {
                                    chart.primary_kind = current_kind;
                                }
                            }
                        }
                    }
                    b"lineChart" | b"line3DChart" => {
                        current_kind = ChartKind::Line;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"areaChart" | b"area3DChart" => {
                        current_kind = ChartKind::Area;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"pieChart" | b"pie3DChart" | b"ofPieChart" => {
                        current_kind = ChartKind::Pie;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"doughnutChart" => {
                        current_kind = ChartKind::Doughnut;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"scatterChart" => {
                        current_kind = ChartKind::Scatter;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"bubbleChart" => {
                        current_kind = ChartKind::Bubble;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"radarChart" => {
                        current_kind = ChartKind::Radar;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"stockChart" => {
                        current_kind = ChartKind::Stock;
                        if chart.series.is_empty() {
                            chart.primary_kind = current_kind;
                        }
                    }
                    b"holeSize" => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Ok(pct) = val.parse::<u32>()
                        {
                            chart.hole_size_pct = pct.clamp(10, 90);
                        }
                    }
                    b"ser" | b"series" => {
                        if let Some(layout_id) = attr_val(e, b"layoutId") {
                            current_kind = match layout_id.as_str() {
                                "waterfall" => ChartKind::Waterfall,
                                "funnel" => ChartKind::Funnel,
                                "treemap" => ChartKind::Treemap,
                                "sunburst" => ChartKind::Sunburst,
                                "histogram" | "paretoLine" => ChartKind::Histogram,
                                "boxWhisker" => ChartKind::BoxWhisker,
                                _ => current_kind,
                            };
                            if chart.series.is_empty() {
                                chart.primary_kind = current_kind;
                            }
                        }
                        current_series = Some(ChartSeries {
                            kind: current_kind,
                            ..ChartSeries::default()
                        });
                    }
                    b"tx" if current_series.is_some() && !in_title => {
                        in_ser_tx = true;
                    }
                    b"cat" | b"ptDim" if current_series.is_some() => {
                        let is_cat =
                            local == b"cat" || attr_val(e, b"type").is_some_and(|t| t == "cat");
                        if is_cat {
                            in_cat = true;
                            in_val = false;
                            in_xval = false;
                            in_bubble = false;
                        }
                    }
                    b"val" | b"numDim" if current_series.is_some() => {
                        if local == b"numDim" || attr_val(e, b"val").is_none() {
                            in_val = true;
                            in_cat = false;
                            in_xval = false;
                            in_bubble = false;
                        }
                    }
                    b"xVal" if current_series.is_some() => {
                        in_xval = true;
                        in_cat = false;
                        in_val = false;
                        in_bubble = false;
                    }
                    b"yVal" if current_series.is_some() => {
                        in_val = true;
                        in_cat = false;
                        in_xval = false;
                        in_bubble = false;
                    }
                    b"bubbleSize" if current_series.is_some() => {
                        in_bubble = true;
                        in_cat = false;
                        in_val = false;
                        in_xval = false;
                    }
                    b"v" | b"lvl" => {
                        if local == b"v" {
                            in_pt_v = true;
                        }
                    }
                    b"pt" if current_series.is_some() && !in_ser_tx => {
                        // cx:pt inside cx:data can have text directly
                        in_pt_v = true;
                        pt_idx = attr_val(e, b"idx").and_then(|v| v.parse::<usize>().ok());
                    }
                    b"formatCode" => {
                        in_format_code = true;
                    }
                    b"srgbClr" if current_series.is_some() => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Some(rgba) = parse_hex_rgba(&val)
                            && let Some(ref mut ser) = current_series
                            && ser.color.is_none()
                        {
                            ser.color = Some(rgba);
                        }
                    }
                    b"explosion" if current_series.is_some() => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Ok(pct) = val.parse::<u32>()
                            && let Some(ref mut ser) = current_series
                        {
                            ser.explosion_pct = pct.min(100);
                        }
                    }
                    b"trendline" if current_series.is_some() => {
                        if let Some(ref mut ser) = current_series {
                            ser.has_linear_trendline = true;
                        }
                    }
                    b"dLbls" => in_dlbls = true,
                    b"showVal" if in_dlbls => {
                        let on = attr_val(e, b"val").is_none_or(|v| v != "0" && v != "false");
                        if let Some(ref mut ser) = current_series {
                            ser.show_data_labels = on;
                        }
                    }
                    b"legend" => {
                        if chart.legend == LegendPosition::None {
                            chart.legend = LegendPosition::Right;
                        }
                    }
                    b"legendPos" => {
                        if let Some(val) = attr_val(e, b"val") {
                            chart.legend = match val.as_str() {
                                "b" => LegendPosition::Bottom,
                                "t" => LegendPosition::Top,
                                "l" => LegendPosition::Left,
                                "tr" => LegendPosition::TopRight,
                                _ => LegendPosition::Right,
                            };
                        }
                    }
                    b"valAx" => in_val_ax = true,
                    b"min" if in_val_ax => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Ok(v) = val.parse::<f64>()
                        {
                            chart.value_axis.min = Some(v);
                        }
                    }
                    b"max" if in_val_ax => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Ok(v) = val.parse::<f64>()
                        {
                            chart.value_axis.max = Some(v);
                        }
                    }
                    b"majorUnit" if in_val_ax => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Ok(v) = val.parse::<f64>()
                        {
                            chart.value_axis.major_unit = Some(v);
                        }
                    }
                    b"logBase" if in_val_ax => {
                        if let Some(val) = attr_val(e, b"val")
                            && let Ok(v) = val.parse::<f64>()
                        {
                            chart.value_axis.log_base = Some(v);
                        }
                    }
                    b"orientation" if in_val_ax => {
                        if let Some(val) = attr_val(e, b"val") {
                            chart.value_axis.reversed = val == "maxMin";
                        }
                    }
                    b"numFmt" if in_val_ax => {
                        if let Some(code) = attr_val(e, b"formatCode") {
                            chart.value_axis.format_code = Some(code);
                        }
                    }
                    b"externalData" => {
                        chart.external_data_ref = attr_val(e, b"id");
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref t)) => {
                let unescaped = quick_xml::escape::unescape(t.as_ref())
                    .map_or_else(|_| t.as_ref().to_owned(), std::borrow::Cow::into_owned);
                let text = unescaped.as_str();
                if in_title {
                    // Separate title runs (`a:t`), but not the pieces of one run around a
                    // reference such as `&amp;`.
                    if title_run_ended && !title_text.is_empty() {
                        title_text.push(' ');
                    }
                    title_run_ended = false;
                    title_text.push_str(text.trim());
                } else if in_format_code {
                    if let Some(ref mut ser) = current_series {
                        ser.format_code = Some(text.trim().to_string());
                    }
                } else if in_pt_v && let Some(ref mut ser) = current_series {
                    let trimmed = text.trim();
                    if in_ser_tx {
                        ser.name = Some(trimmed.to_string());
                    } else if in_cat {
                        store_point(
                            &mut ser.categories,
                            pt_idx,
                            trimmed.to_string(),
                            String::new(),
                        );
                    } else if in_val {
                        let num = trimmed.parse::<f64>().unwrap_or(f64::NAN);
                        store_point(&mut ser.values, pt_idx, num, f64::NAN);
                    } else if in_xval {
                        let num = trimmed.parse::<f64>().unwrap_or(f64::NAN);
                        store_point(&mut ser.x_values, pt_idx, num, f64::NAN);
                    } else if in_bubble {
                        let num = trimmed.parse::<f64>().unwrap_or(f64::NAN);
                        store_point(&mut ser.bubble_sizes, pt_idx, num, f64::NAN);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let qname = e.name();
                match local_name(qname.as_ref()) {
                    b"title" => {
                        in_title = false;
                        if !title_text.is_empty() && chart.title.is_none() {
                            chart.title = Some(std::mem::take(&mut title_text));
                        }
                    }
                    b"ser" | b"series" => {
                        if let Some(ser) = current_series.take() {
                            chart.series.push(ser);
                        }
                        in_ser_tx = false;
                        in_cat = false;
                        in_val = false;
                        in_xval = false;
                        in_bubble = false;
                    }
                    b"tx" => in_ser_tx = false,
                    b"cat" | b"ptDim" => in_cat = false,
                    b"val" | b"yVal" | b"numDim" => in_val = false,
                    b"xVal" => in_xval = false,
                    b"bubbleSize" => in_bubble = false,
                    b"v" => in_pt_v = false,
                    b"t" => title_run_ended = true,
                    b"pt" => {
                        in_pt_v = false;
                        pt_idx = None;
                    }
                    b"formatCode" => in_format_code = false,
                    b"dLbls" => in_dlbls = false,
                    b"valAx" => in_val_ax = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::ParseError(format!("Malformed chart XML: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    Ok(chart)
}

/// Store a cached point at its `idx` (cache points are sparse: blank cells have no `c:pt`), or
/// append it when it has no index. Skipped indices are filled with `gap` (`NaN` for numbers,
/// which renderers draw as gaps).
fn store_point<T: Clone>(list: &mut Vec<T>, idx: Option<usize>, value: T, gap: T) {
    let at = idx.unwrap_or(list.len());
    if at >= MAX_CHART_POINTS {
        return;
    }
    if at < list.len() {
        list[at] = value;
    } else {
        list.resize(at, gap);
        list.push(value);
    }
}

fn resolve_bar_kind(is_bar: bool, grouping: &str) -> ChartKind {
    match (is_bar, grouping) {
        (false, "stacked") => ChartKind::ColumnStacked,
        (false, "percentStacked") => ChartKind::ColumnPercentStacked,
        (false, _) => ChartKind::ColumnClustered,
        (true, "stacked") => ChartKind::BarStacked,
        (true, "percentStacked") => ChartKind::BarPercentStacked,
        (true, _) => ChartKind::BarClustered,
    }
}

/// Render a [`ChartSpace`] into positioned [`Item`]s inside `bounds`.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn render_chart(
    chart: &ChartSpace,
    bounds: Rect,
    theme: &DrawingTheme,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) -> Vec<Item> {
    if bounds.width < 12.0 || bounds.height < 12.0 {
        return Vec::new();
    }

    let mut items = Vec::new();

    // Chart background + outer frame
    items.push(Item::Path(PathItem {
        commands: rect_cmds(bounds),
        fill: Some(FillPaint::Solid(Rgba::WHITE)),
        stroke: Some(thin_stroke(Rgba::rgb(0xD9, 0xD9, 0xD9), 0.75)),
        fill_rule: FillRule::NonZero,
        transform: Affine::IDENTITY,
    }));

    // Title
    let mut top_pad = 10.0_f32;
    if let Some(ref title) = chart.title
        && !title.is_empty()
    {
        emit_label(
            &mut items,
            title,
            Point::new(bounds.x + 8.0, bounds.y + 6.0),
            Some((bounds.width - 16.0).max(20.0)),
            11.0,
            true,
            Rgba::rgb(0x33, 0x33, 0x33),
            TextAlign::Center,
            &theme.major_latin_font,
            font_sys,
            text_engine,
        );
        top_pad = 26.0;
    }

    if !chart.has_cached_values() {
        emit_label(
            &mut items,
            "[Chart: no cached series values]",
            Point::new(bounds.x + 8.0, bounds.y + bounds.height * 0.45),
            Some((bounds.width - 16.0).max(20.0)),
            9.0,
            false,
            Rgba::rgb(0x7F, 0x7F, 0x7F),
            TextAlign::Center,
            &theme.minor_latin_font,
            font_sys,
            text_engine,
        );
        return items;
    }

    let palette = theme.accent_palette();
    let right_legend_w = if matches!(
        chart.legend,
        LegendPosition::Right | LegendPosition::TopRight
    ) && chart.series.len() > 1
    {
        (bounds.width * 0.22).clamp(48.0, 96.0)
    } else {
        8.0
    };
    let bottom_legend_h = if chart.legend == LegendPosition::Bottom {
        18.0
    } else {
        0.0
    };

    let plot_rect = Rect::new(
        bounds.x + 36.0,
        bounds.y + top_pad,
        (bounds.width - 44.0 - right_legend_w).max(24.0),
        (bounds.height - top_pad - 22.0 - bottom_legend_h).max(24.0),
    );

    match chart.primary_kind {
        ChartKind::Pie | ChartKind::Doughnut | ChartKind::Sunburst => {
            render_pie_or_doughnut(
                chart,
                plot_rect,
                &palette,
                &theme.minor_latin_font,
                font_sys,
                text_engine,
                &mut items,
            );
        }
        ChartKind::Treemap => {
            render_treemap(
                chart,
                plot_rect,
                &palette,
                &theme.minor_latin_font,
                font_sys,
                text_engine,
                &mut items,
            );
        }
        ChartKind::Funnel => {
            render_funnel(
                chart,
                plot_rect,
                &palette,
                &theme.minor_latin_font,
                font_sys,
                text_engine,
                &mut items,
            );
        }
        _ => {
            render_cartesian_chart(
                chart,
                plot_rect,
                &palette,
                &theme.minor_latin_font,
                font_sys,
                text_engine,
                &mut items,
            );
        }
    }

    // Legend
    if chart.legend != LegendPosition::None {
        render_legend(
            chart,
            bounds,
            plot_rect,
            &palette,
            &theme.minor_latin_font,
            font_sys,
            text_engine,
            &mut items,
        );
    }

    items
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn render_cartesian_chart(
    chart: &ChartSpace,
    plot: Rect,
    palette: &[Rgba; 6],
    font_family: &str,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let is_stacked_100 = matches!(
        chart.primary_kind,
        ChartKind::ColumnPercentStacked | ChartKind::BarPercentStacked
    );
    let is_stacked = matches!(
        chart.primary_kind,
        ChartKind::ColumnStacked | ChartKind::BarStacked
    ) || is_stacked_100;

    let max_pts = chart
        .series
        .iter()
        .map(|s| s.values.len())
        .max()
        .unwrap_or(0);

    let all_vals: Vec<f64> = if is_stacked {
        (0..max_pts)
            .map(|pt_i| {
                chart
                    .series
                    .iter()
                    .filter_map(|s| s.values.get(pt_i).copied())
                    .filter(|v| v.is_finite())
                    .sum::<f64>()
            })
            .collect()
    } else {
        chart
            .series
            .iter()
            .flat_map(|s| s.values.iter().copied())
            .collect()
    };

    let scale = compute_excel_axis_scale(
        &all_vals,
        chart.value_axis.min,
        chart.value_axis.max,
        chart.value_axis.major_unit,
        true,
        is_stacked_100,
    );

    let val_span = (scale.max - scale.min).max(1e-6);
    let map_y = |v: f64| -> f32 {
        let norm = ((v - scale.min) / val_span).clamp(0.0, 1.0) as f32;
        if chart.value_axis.reversed {
            plot.y + norm * plot.height
        } else {
            plot.bottom() - norm * plot.height
        }
    };
    let zero_y = map_y(0.0_f64.clamp(scale.min, scale.max));

    // Gridlines and Y-axis tick labels
    let fmt_opts = FormatOptions::default();
    let tick_fmt = chart
        .value_axis
        .format_code
        .as_deref()
        .or_else(|| chart.series.first().and_then(|s| s.format_code.as_deref()))
        .unwrap_or(if is_stacked_100 { "0%" } else { "General" });

    let num_steps = ((scale.max - scale.min) / scale.major_unit).round() as usize;
    for step in 0..=num_steps.min(12) {
        let tick_val = scale.min + (step as f64) * scale.major_unit;
        let ty = map_y(tick_val);
        if chart.value_axis.show_major_gridlines {
            items.push(Item::Path(PathItem {
                commands: vec![
                    PathCommand::MoveTo(Point::new(plot.x, ty)),
                    PathCommand::LineTo(Point::new(plot.right(), ty)),
                ],
                fill: None,
                stroke: Some(thin_stroke(Rgba::rgb(0xE5, 0xE7, 0xEB), 0.5)),
                fill_rule: FillRule::NonZero,
                transform: Affine::IDENTITY,
            }));
        }
        let label_str =
            format_with_code(&CellInputValue::Number(tick_val), tick_fmt, &fmt_opts).text;
        emit_label(
            items,
            &label_str,
            Point::new(plot.x - 34.0, ty - 4.0),
            Some(30.0),
            7.0,
            false,
            Rgba::rgb(0x59, 0x59, 0x59),
            TextAlign::Right,
            font_family,
            font_sys,
            text_engine,
        );
    }

    // Category labels along X axis
    if let Some(first_ser) = chart.series.first()
        && max_pts > 0
    {
        let group_w = plot.width / (max_pts as f32);
        for i in 0..max_pts {
            let cat_str = first_ser
                .categories
                .get(i)
                .cloned()
                .unwrap_or_else(|| (i + 1).to_string());
            let gx = plot.x + (i as f32) * group_w;
            emit_label(
                items,
                &cat_str,
                Point::new(gx, plot.bottom() + 3.0),
                Some(group_w),
                7.0,
                false,
                Rgba::rgb(0x59, 0x59, 0x59),
                TextAlign::Center,
                font_family,
                font_sys,
                text_engine,
            );
        }
    }

    // Plot series
    let num_ser = chart.series.len().max(1);
    let group_w = plot.width / (max_pts.max(1) as f32);
    let mut stacked_pos_y = vec![zero_y; max_pts];

    for (s_idx, ser) in chart.series.iter().enumerate() {
        let color = ser.color.unwrap_or(palette[s_idx % palette.len()]);
        match ser.kind {
            ChartKind::Line | ChartKind::Radar | ChartKind::Stock => {
                let mut line_cmds = Vec::new();
                let mut pen_down = false;
                for (i, &val) in ser.values.iter().enumerate() {
                    // A blank point is a gap in the line (`c:dispBlanksAs` default).
                    if !val.is_finite() {
                        pen_down = false;
                        continue;
                    }
                    let px = plot.x + ((i as f32) + 0.5) * group_w;
                    let py = map_y(val);
                    if pen_down {
                        line_cmds.push(PathCommand::LineTo(Point::new(px, py)));
                    } else {
                        line_cmds.push(PathCommand::MoveTo(Point::new(px, py)));
                        pen_down = true;
                    }
                    // Marker dot
                    items.push(Item::Path(PathItem {
                        commands: rect_cmds(Rect::new(px - 2.0, py - 2.0, 4.0, 4.0)),
                        fill: Some(FillPaint::Solid(color)),
                        stroke: None,
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }
                if line_cmds.len() >= 2 {
                    items.push(Item::Path(PathItem {
                        commands: line_cmds,
                        fill: None,
                        stroke: Some(thin_stroke(color, 1.75)),
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }
            }
            ChartKind::Area => {
                if !ser.values.is_empty() {
                    let mut cmds = Vec::new();
                    let first_x = plot.x + 0.5 * group_w;
                    cmds.push(PathCommand::MoveTo(Point::new(first_x, zero_y)));
                    for (i, &val) in ser.values.iter().enumerate() {
                        let px = plot.x + ((i as f32) + 0.5) * group_w;
                        let val = if val.is_finite() { val } else { 0.0 };
                        cmds.push(PathCommand::LineTo(Point::new(px, map_y(val))));
                    }
                    let last_x = plot.x + ((ser.values.len() as f32) - 0.5) * group_w;
                    cmds.push(PathCommand::LineTo(Point::new(last_x, zero_y)));
                    cmds.push(PathCommand::Close);
                    let mut fill_c = color;
                    fill_c.a = 170;
                    items.push(Item::Path(PathItem {
                        commands: cmds,
                        fill: Some(FillPaint::Solid(fill_c)),
                        stroke: Some(thin_stroke(color, 1.25)),
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }
            }
            ChartKind::Scatter | ChartKind::Bubble => {
                for (i, &val) in ser.values.iter().enumerate() {
                    if !val.is_finite() {
                        continue;
                    }
                    let px = plot.x + ((i as f32) + 0.5) * group_w;
                    let py = map_y(val);
                    let r = ser
                        .bubble_sizes
                        .get(i)
                        .map_or(3.0, |&b| (b.abs().sqrt() as f32).clamp(3.0, 14.0));
                    items.push(Item::Path(PathItem {
                        commands: rect_cmds(Rect::new(px - r, py - r, r * 2.0, r * 2.0)),
                        fill: Some(FillPaint::Solid(color)),
                        stroke: None,
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));
                }
            }
            _ => {
                // Columns / Bars / Waterfall / Histogram / BoxWhisker
                let bar_band = group_w * 0.72;
                let single_w = if is_stacked {
                    bar_band
                } else {
                    (bar_band / (num_ser as f32)).max(2.0)
                };
                for (i, &val) in ser.values.iter().enumerate() {
                    if !val.is_finite() {
                        continue;
                    }
                    let gx = plot.x + (i as f32) * group_w + (group_w - bar_band) * 0.5;
                    let bx = if is_stacked {
                        gx
                    } else {
                        gx + (s_idx as f32) * single_w
                    };
                    let eff_val = if is_stacked_100 {
                        let col_sum: f64 = chart
                            .series
                            .iter()
                            .filter_map(|s| s.values.get(i).copied())
                            .filter(|v| v.is_finite())
                            .map(f64::abs)
                            .sum();
                        if col_sum > 0.0 {
                            val.abs() / col_sum
                        } else {
                            0.0
                        }
                    } else {
                        val
                    };

                    let (top_y, bot_y) = if is_stacked {
                        let base_y = stacked_pos_y[i];
                        let h_px = ((eff_val.abs() / val_span) as f32) * plot.height;
                        let new_top = (base_y - h_px).max(plot.y);
                        stacked_pos_y[i] = new_top;
                        (new_top, base_y)
                    } else {
                        let vy = map_y(eff_val);
                        (vy.min(zero_y), vy.max(zero_y))
                    };

                    let rect_h = (bot_y - top_y).max(1.0);
                    items.push(Item::Path(PathItem {
                        commands: rect_cmds(Rect::new(bx, top_y, single_w, rect_h)),
                        fill: Some(FillPaint::Solid(color)),
                        stroke: None,
                        fill_rule: FillRule::NonZero,
                        transform: Affine::IDENTITY,
                    }));

                    if ser.show_data_labels {
                        let lbl =
                            format_with_code(&CellInputValue::Number(val), tick_fmt, &fmt_opts)
                                .text;
                        emit_label(
                            items,
                            &lbl,
                            Point::new(bx - 4.0, (top_y - 10.0).max(plot.y)),
                            Some(single_w + 8.0),
                            6.5,
                            false,
                            Rgba::rgb(0x33, 0x33, 0x33),
                            TextAlign::Center,
                            font_family,
                            font_sys,
                            text_engine,
                        );
                    }
                }
            }
        }
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn render_pie_or_doughnut(
    chart: &ChartSpace,
    plot: Rect,
    palette: &[Rgba; 6],
    _font_family: &str,
    _font_sys: &mut FontSystem,
    _text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let Some(ser) = chart.series.first() else {
        return;
    };
    let total: f64 = ser
        .values
        .iter()
        .filter(|v| v.is_finite())
        .map(|v| v.abs())
        .sum();
    if total <= 0.0 {
        return;
    }

    let r = (plot.width.min(plot.height) * 0.44).max(10.0);
    let cx = plot.x + plot.width * 0.5;
    let cy = plot.y + plot.height * 0.5;
    let mut start_ang = -std::f32::consts::FRAC_PI_2;

    for (i, &val) in ser.values.iter().enumerate() {
        if !val.is_finite() {
            continue;
        }
        let sweep = ((val.abs() / total) as f32) * TAU;
        if sweep <= 0.001 {
            continue;
        }
        let color = palette[i % palette.len()];
        let steps = ((sweep / 0.15).ceil() as usize).clamp(2, 32);
        let mut cmds = Vec::with_capacity(steps + 3);
        cmds.push(PathCommand::MoveTo(Point::new(cx, cy)));
        for s in 0..=steps {
            let ang = start_ang + sweep * ((s as f32) / (steps as f32));
            cmds.push(PathCommand::LineTo(Point::new(
                cx + r * ang.cos(),
                cy + r * ang.sin(),
            )));
        }
        cmds.push(PathCommand::Close);
        items.push(Item::Path(PathItem {
            commands: cmds,
            fill: Some(FillPaint::Solid(color)),
            stroke: Some(thin_stroke(Rgba::WHITE, 1.0)),
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
        start_ang += sweep;
    }

    if matches!(
        chart.primary_kind,
        ChartKind::Doughnut | ChartKind::Sunburst
    ) {
        let hole_r = r * ((chart.hole_size_pct as f32) / 100.0);
        let steps = 24usize;
        let mut hole_cmds = Vec::with_capacity(steps + 2);
        for s in 0..steps {
            let ang = ((s as f32) / (steps as f32)) * TAU;
            let pt = Point::new(cx + hole_r * ang.cos(), cy + hole_r * ang.sin());
            if s == 0 {
                hole_cmds.push(PathCommand::MoveTo(pt));
            } else {
                hole_cmds.push(PathCommand::LineTo(pt));
            }
        }
        hole_cmds.push(PathCommand::Close);
        items.push(Item::Path(PathItem {
            commands: hole_cmds,
            fill: Some(FillPaint::Solid(Rgba::WHITE)),
            stroke: None,
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn render_treemap(
    chart: &ChartSpace,
    plot: Rect,
    palette: &[Rgba; 6],
    font_family: &str,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let Some(ser) = chart.series.first() else {
        return;
    };
    let total: f64 = ser
        .values
        .iter()
        .filter(|v| v.is_finite())
        .map(|v| v.abs())
        .sum();
    if total <= 0.0 {
        return;
    }
    let mut cur_x = plot.x;
    for (i, &val) in ser.values.iter().enumerate() {
        if !val.is_finite() {
            continue;
        }
        let w = (((val.abs() / total) as f32) * plot.width).max(4.0);
        let rect = Rect::new(cur_x, plot.y, w.min(plot.right() - cur_x), plot.height);
        let color = palette[i % palette.len()];
        items.push(Item::Path(PathItem {
            commands: rect_cmds(rect),
            fill: Some(FillPaint::Solid(color)),
            stroke: Some(thin_stroke(Rgba::WHITE, 1.5)),
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
        if let Some(cat) = ser.categories.get(i)
            && rect.width > 24.0
        {
            emit_label(
                items,
                cat,
                Point::new(rect.x + 4.0, rect.y + 4.0),
                Some(rect.width - 8.0),
                7.5,
                true,
                Rgba::WHITE,
                TextAlign::Left,
                font_family,
                font_sys,
                text_engine,
            );
        }
        cur_x += w;
        if cur_x >= plot.right() {
            break;
        }
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn render_funnel(
    chart: &ChartSpace,
    plot: Rect,
    palette: &[Rgba; 6],
    font_family: &str,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let Some(ser) = chart.series.first() else {
        return;
    };
    let max_val = ser
        .values
        .iter()
        .copied()
        .map(f64::abs)
        .fold(0.0_f64, f64::max);
    if max_val <= 0.0 || ser.values.is_empty() {
        return;
    }
    let row_h = plot.height / (ser.values.len() as f32);
    for (i, &val) in ser.values.iter().enumerate() {
        if !val.is_finite() {
            continue;
        }
        let w = (((val.abs() / max_val) as f32) * plot.width).max(8.0);
        let rx = plot.x + (plot.width - w) * 0.5;
        let ry = plot.y + (i as f32) * row_h;
        let color = palette[i % palette.len()];
        items.push(Item::Path(PathItem {
            commands: rect_cmds(Rect::new(rx, ry, w, (row_h - 2.0).max(2.0))),
            fill: Some(FillPaint::Solid(color)),
            stroke: None,
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
        if let Some(cat) = ser.categories.get(i) {
            emit_label(
                items,
                cat,
                Point::new(plot.x - 34.0, ry + 2.0),
                Some(32.0),
                7.0,
                false,
                Rgba::rgb(0x59, 0x59, 0x59),
                TextAlign::Right,
                font_family,
                font_sys,
                text_engine,
            );
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::cast_precision_loss)]
fn render_legend(
    chart: &ChartSpace,
    bounds: Rect,
    plot: Rect,
    palette: &[Rgba; 6],
    font_family: &str,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
    items: &mut Vec<Item>,
) {
    let lx = plot.right() + 8.0;
    let mut ly = plot.y + 4.0;
    let avail_w = (bounds.right() - lx - 4.0).max(24.0);
    if avail_w < 24.0 {
        return;
    }

    for (i, ser) in chart.series.iter().enumerate() {
        let color = ser.color.unwrap_or(palette[i % palette.len()]);
        let label = ser
            .name
            .clone()
            .unwrap_or_else(|| format!("Series {}", i + 1));
        items.push(Item::Path(PathItem {
            commands: rect_cmds(Rect::new(lx, ly + 2.0, 7.0, 7.0)),
            fill: Some(FillPaint::Solid(color)),
            stroke: None,
            fill_rule: FillRule::NonZero,
            transform: Affine::IDENTITY,
        }));
        emit_label(
            items,
            &label,
            Point::new(lx + 10.0, ly),
            Some(avail_w - 10.0),
            7.5,
            false,
            Rgba::rgb(0x33, 0x33, 0x33),
            TextAlign::Left,
            font_family,
            font_sys,
            text_engine,
        );
        ly += 13.0;
        if ly > plot.bottom() {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_label(
    items: &mut Vec<Item>,
    text: &str,
    origin: Point,
    max_width: Option<f32>,
    size_pt: f32,
    bold: bool,
    color: Rgba,
    align: TextAlign,
    family: &str,
    font_sys: &mut FontSystem,
    text_engine: &mut TextLayoutEngine,
) {
    if text.is_empty() {
        return;
    }
    let spec = ParagraphSpec {
        spans: vec![SpanSpec {
            text: text.to_string(),
            family: family.to_string(),
            size: size_pt,
            bold,
            italic: false,
            color,
            run_index: 0,
            ..SpanSpec::default()
        }],
        align,
        line_spacing: LineSpacing::Multiple(1.05),
        max_width,
        first_line_indent: 0.0,
        origin,
        page_index: 0,
        block_index: 0,
    };
    let shaped = text_engine.layout_paragraph(&spec, font_sys);
    for run in shaped.runs {
        items.push(Item::Glyphs(run));
    }
}

fn rect_cmds(r: Rect) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(Point::new(r.x, r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.y)),
        PathCommand::LineTo(Point::new(r.right(), r.bottom())),
        PathCommand::LineTo(Point::new(r.x, r.bottom())),
        PathCommand::Close,
    ]
}

fn thin_stroke(color: Rgba, width: f32) -> StrokeStyle {
    StrokeStyle {
        color,
        width,
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        dash_array: Vec::new(),
        dash_offset: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cached_series_and_renders_chart_with_excel_axis_scale() {
        let xml = r#"<?xml version="1.0"?>
        <c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
          <c:chart>
            <c:title><c:tx><c:rich><a:p xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:r><a:t>Quarterly Revenue</a:t></a:r></a:p></c:rich></c:tx></c:title>
            <c:plotArea>
              <c:barChart>
                <c:barDir val="col"/>
                <c:grouping val="clustered"/>
                <c:ser>
                  <c:tx><c:v>FY25</c:v></c:tx>
                  <c:cat><c:strRef><c:strCache>
                    <c:pt idx="0"><c:v>Q1</c:v></c:pt>
                    <c:pt idx="1"><c:v>Q2</c:v></c:pt>
                  </c:strCache></c:strRef></c:cat>
                  <c:val><c:numRef><c:numCache>
                    <c:formatCode>$#,##0</c:formatCode>
                    <c:pt idx="0"><c:v>120</c:v></c:pt>
                    <c:pt idx="1"><c:v>245</c:v></c:pt>
                  </c:numCache></c:numRef></c:val>
                </c:ser>
              </c:barChart>
              <c:valAx><c:majorGridlines/></c:valAx>
            </c:plotArea>
            <c:legend><c:legendPos val="r"/></c:legend>
          </c:chart>
        </c:chartSpace>"#;

        let chart = parse_chart_xml(xml).expect("parse chart");
        assert_eq!(chart.title.as_deref(), Some("Quarterly Revenue"));
        assert_eq!(chart.series.len(), 1);
        assert_eq!(chart.series[0].categories, vec!["Q1", "Q2"]);
        assert_eq!(chart.series[0].values, vec![120.0, 245.0]);

        let scale =
            compute_excel_axis_scale(&chart.series[0].values, None, None, None, true, false);
        assert_eq!(scale.min, 0.0);
        assert!(scale.max >= 250.0);
        assert!(scale.major_unit >= 20.0);

        let mut font_sys = FontSystem::new(&prism_layout::LayoutOptions::default());
        let mut text_engine = TextLayoutEngine::new();
        let items = render_chart(
            &chart,
            Rect::new(0.0, 0.0, 400.0, 260.0),
            &DrawingTheme::default(),
            &mut font_sys,
            &mut text_engine,
        );
        assert!(!items.is_empty());
    }

    fn bar_chart(points: &str, scaling: &str) -> String {
        format!(
            r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
              <c:chart><c:plotArea><c:barChart><c:barDir val="col"/><c:ser>
                <c:val><c:numRef><c:numCache><c:ptCount val="5"/>{points}</c:numCache></c:numRef></c:val>
              </c:ser></c:barChart><c:valAx><c:scaling>{scaling}</c:scaling></c:valAx></c:plotArea></c:chart>
            </c:chartSpace>"#
        )
    }

    fn render(chart: &ChartSpace) -> Vec<Item> {
        let mut font_sys = FontSystem::new(&prism_layout::LayoutOptions::default());
        let mut text_engine = TextLayoutEngine::new();
        render_chart(
            chart,
            Rect::new(0.0, 0.0, 400.0, 260.0),
            &DrawingTheme::default(),
            &mut font_sys,
            &mut text_engine,
        )
    }

    #[test]
    fn sparse_cache_points_keep_their_index() {
        // Blank cells have no `c:pt`: idx 1 and 3 are gaps, not shifted neighbours.
        let xml = bar_chart(
            r#"<c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="2"><c:v>3</c:v></c:pt><c:pt idx="4"><c:v>5</c:v></c:pt>"#,
            "",
        );
        let chart = parse_chart_xml(&xml).unwrap();
        let v = &chart.series[0].values;
        assert_eq!(v.len(), 5);
        assert_eq!((v[0], v[2], v[4]), (1.0, 3.0, 5.0));
        assert!(v[1].is_nan() && v[3].is_nan());
        assert!(!render(&chart).is_empty());
    }

    #[test]
    fn non_finite_axis_bounds_do_not_panic() {
        for scaling in [
            r#"<c:min val="NaN"/>"#,
            r#"<c:max val="NaN"/><c:min val="inf"/>"#,
            r#"<c:min val="100"/><c:max val="-100"/>"#,
        ] {
            let xml = bar_chart(
                r#"<c:pt idx="0"><c:v>1e308</c:v></c:pt><c:pt idx="1"><c:v>-1e308</c:v></c:pt><c:pt idx="2"><c:v>NaN</c:v></c:pt>"#,
                scaling,
            );
            let chart = parse_chart_xml(&xml).unwrap();
            let scale = compute_excel_axis_scale(
                &chart.series[0].values,
                chart.value_axis.min,
                chart.value_axis.max,
                chart.value_axis.major_unit,
                true,
                false,
            );
            assert!(scale.min.is_finite() && scale.max > scale.min, "{scaling}");
            assert!(!render(&chart).is_empty(), "{scaling}");
        }
    }
}
