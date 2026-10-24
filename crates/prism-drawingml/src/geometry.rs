// SPDX-License-Identifier: AGPL-3.0-only
//! ECMA-376 Part 1 (§20.1.9) Preset Shape Geometry (`a:prstGeom`), Guide Formula
//! Evaluator (`a:avLst` / `a:gdLst`), and Custom Geometry (`a:custGeom`).

use prism_layout::model::{PathCommand, Point, Rect};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::f64::consts::PI;

/// `DrawingML` angle unit: 60,000ths of a degree (`21,600,000` = 360°).
pub const ANGLE_UNITS_PER_DEG: f64 = 60_000.0;
/// Full circle in `DrawingML` angle units (`360 * 60,000`).
pub const FULL_CIRCLE_UNITS: f64 = 21_600_000.0;

/// All 187 standard ECMA-376 `ST_ShapeType` preset geometry names.
pub const ALL_PRESET_SHAPES: [&str; 187] = [
    "line",
    "lineInv",
    "triangle",
    "rtTriangle",
    "rect",
    "diamond",
    "parallelogram",
    "trapezoid",
    "nonIsoscelesTrapezoid",
    "pentagon",
    "hexagon",
    "heptagon",
    "octagon",
    "decagon",
    "dodecagon",
    "star4",
    "star5",
    "star6",
    "star7",
    "star8",
    "star10",
    "star12",
    "star16",
    "star24",
    "star32",
    "roundRect",
    "round1Rect",
    "round2SameRect",
    "round2DiagRect",
    "snipRoundRect",
    "snip1Rect",
    "snip2SameRect",
    "snip2DiagRect",
    "plaque",
    "ellipse",
    "teardrop",
    "homePlate",
    "chevron",
    "pieWedge",
    "pie",
    "blockArc",
    "donut",
    "noSmoking",
    "rightArrow",
    "leftArrow",
    "upArrow",
    "downArrow",
    "stripedRightArrow",
    "notchedRightArrow",
    "bentUpArrow",
    "leftRightArrow",
    "upDownArrow",
    "leftUpArrow",
    "leftRightUpArrow",
    "quadArrow",
    "leftArrowCallout",
    "rightArrowCallout",
    "upArrowCallout",
    "downArrowCallout",
    "leftRightArrowCallout",
    "upDownArrowCallout",
    "quadArrowCallout",
    "bentArrow",
    "uturnArrow",
    "circularArrow",
    "leftCircularArrow",
    "leftRightCircularArrow",
    "curvedRightArrow",
    "curvedLeftArrow",
    "curvedUpArrow",
    "curvedDownArrow",
    "swooshArrow",
    "cube",
    "can",
    "lightningBolt",
    "heart",
    "sun",
    "moon",
    "smileyFace",
    "irregularSeal1",
    "irregularSeal2",
    "foldedCorner",
    "bevel",
    "frame",
    "halfFrame",
    "corner",
    "diagStripe",
    "chord",
    "arc",
    "leftBracket",
    "rightBracket",
    "leftBrace",
    "rightBrace",
    "bracketPair",
    "bracePair",
    "straightConnector1",
    "bentConnector2",
    "bentConnector3",
    "bentConnector4",
    "bentConnector5",
    "curvedConnector2",
    "curvedConnector3",
    "curvedConnector4",
    "curvedConnector5",
    "callout1",
    "callout2",
    "callout3",
    "accentCallout1",
    "accentCallout2",
    "accentCallout3",
    "borderCallout1",
    "borderCallout2",
    "borderCallout3",
    "accentBorderCallout1",
    "accentBorderCallout2",
    "accentBorderCallout3",
    "wedgeRectCallout",
    "wedgeRoundRectCallout",
    "wedgeEllipseCallout",
    "cloudCallout",
    "cloud",
    "ribbon",
    "ribbon2",
    "ellipseRibbon",
    "ellipseRibbon2",
    "leftRightRibbon",
    "verticalScroll",
    "horizontalScroll",
    "wave",
    "doubleWave",
    "plus",
    "flowChartProcess",
    "flowChartDecision",
    "flowChartInputOutput",
    "flowChartPredefinedProcess",
    "flowChartInternalStorage",
    "flowChartDocument",
    "flowChartMultidocument",
    "flowChartTerminator",
    "flowChartPreparation",
    "flowChartManualInput",
    "flowChartManualOperation",
    "flowChartConnector",
    "flowChartPunchedCard",
    "flowChartPunchedTape",
    "flowChartSummingJunction",
    "flowChartOr",
    "flowChartCollate",
    "flowChartSort",
    "flowChartExtract",
    "flowChartMerge",
    "flowChartOfflineStorage",
    "flowChartOnlineStorage",
    "flowChartMagneticTape",
    "flowChartMagneticDisk",
    "flowChartMagneticDrum",
    "flowChartDisplay",
    "flowChartDelay",
    "flowChartAlternateProcess",
    "flowChartOffpageConnector",
    "actionButtonBlank",
    "actionButtonHome",
    "actionButtonHelp",
    "actionButtonInformation",
    "actionButtonForwardNext",
    "actionButtonBackPrevious",
    "actionButtonEnd",
    "actionButtonBeginning",
    "actionButtonReturn",
    "actionButtonDocument",
    "actionButtonSound",
    "actionButtonMovie",
    "gear6",
    "gear9",
    "funnel",
    "mathPlus",
    "mathMinus",
    "mathMultiply",
    "mathDivide",
    "mathEqual",
    "mathNotEqual",
    "cornerTabs",
    "squareTabs",
    "plaqueTabs",
    "chartX",
    "chartStar",
    "chartPlus",
];

/// Returns `true` if `name` is one of the 187 standard ECMA-376 preset shape types.
#[must_use]
pub fn is_known_preset_shape(name: &str) -> bool {
    ALL_PRESET_SHAPES.contains(&name)
}

/// Guide definition (`<a:gd name="..." fmla="..."/>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuideDef {
    /// Variable name bound by this guide.
    pub name: String,
    /// Formula string (e.g. `"val 16667"`, `"*/ w adj 100000"`, `"+- r 0 dx"`).
    pub fmla: String,
}

/// Command inside a `DrawingML` `<a:path>` (`a:custGeom`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustomPathCmd {
    /// `<a:moveTo><a:pt x="..." y="..."/></a:moveTo>`
    MoveTo {
        /// X coordinate or guide variable name.
        x: String,
        /// Y coordinate or guide variable name.
        y: String,
    },
    /// `<a:lnTo><a:pt x="..." y="..."/></a:lnTo>`
    LineTo {
        /// X coordinate or guide variable name.
        x: String,
        /// Y coordinate or guide variable name.
        y: String,
    },
    /// `<a:cubicBezTo>` with 3 control/target points.
    CubicBezTo {
        /// First control point `(x, y)`.
        pt1: (String, String),
        /// Second control point `(x, y)`.
        pt2: (String, String),
        /// Endpoint `(x, y)`.
        pt3: (String, String),
    },
    /// `<a:quadBezTo>` with 1 control point and 1 endpoint.
    QuadBezTo {
        /// Control point `(x, y)`.
        pt1: (String, String),
        /// Endpoint `(x, y)`.
        pt2: (String, String),
    },
    /// `<a:arcTo wR="..." hR="..." stAng="..." swAng="..."/>`
    ArcTo {
        /// Horizontal radius or guide name.
        wr: String,
        /// Vertical radius or guide name.
        hr: String,
        /// Start angle in 60,000ths of a degree or guide name.
        st_ang: String,
        /// Swing angle in 60,000ths of a degree or guide name.
        sw_ang: String,
    },
    /// `<a:close/>`
    Close,
}

/// A single `<a:path>` inside a `<a:pathLst>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomPath {
    /// Coordinate system width (`@w`), or `0` to use shape width.
    pub width: i64,
    /// Coordinate system height (`@h`), or `0` to use shape height.
    pub height: i64,
    /// Path commands.
    pub commands: Vec<CustomPathCmd>,
}

/// Parsed `<a:custGeom>` custom shape geometry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomGeometry {
    /// Adjust values (`a:avLst`).
    pub adjust_values: Vec<GuideDef>,
    /// Guide formulas (`a:gdLst`).
    pub guides: Vec<GuideDef>,
    /// Sub-paths (`a:pathLst`).
    pub paths: Vec<CustomPath>,
}

/// Populate the built-in ECMA-376 shape coordinate variables for a bounding box of `(w, h)`.
#[must_use]
pub fn builtin_shape_vars(w: f64, h: f64) -> BTreeMap<String, f64> {
    let mut vars = BTreeMap::new();
    let ss = w.min(h);
    let ls = w.max(h);
    vars.insert("w".to_string(), w);
    vars.insert("h".to_string(), h);
    vars.insert("l".to_string(), 0.0);
    vars.insert("t".to_string(), 0.0);
    vars.insert("r".to_string(), w);
    vars.insert("b".to_string(), h);
    vars.insert("hc".to_string(), w * 0.5);
    vars.insert("vc".to_string(), h * 0.5);
    vars.insert("ss".to_string(), ss);
    vars.insert("ls".to_string(), ls);

    for div in [2u32, 3, 4, 5, 6, 8, 10, 12, 16, 32] {
        let d = f64::from(div);
        vars.insert(format!("wd{div}"), w / d);
        vars.insert(format!("hd{div}"), h / d);
        vars.insert(format!("ssd{div}"), ss / d);
    }

    vars.insert("cd2".to_string(), FULL_CIRCLE_UNITS / 2.0);
    vars.insert("cd4".to_string(), FULL_CIRCLE_UNITS / 4.0);
    vars.insert("cd8".to_string(), FULL_CIRCLE_UNITS / 8.0);
    vars.insert("3cd4".to_string(), 3.0 * FULL_CIRCLE_UNITS / 4.0);
    vars.insert("3cd8".to_string(), 3.0 * FULL_CIRCLE_UNITS / 8.0);
    vars.insert("5cd8".to_string(), 5.0 * FULL_CIRCLE_UNITS / 8.0);
    vars.insert("7cd8".to_string(), 7.0 * FULL_CIRCLE_UNITS / 8.0);
    vars
}

/// Evaluate a single ECMA-376 §20.1.9.11 `DrawingML` guide formula against `vars`.
#[must_use]
pub fn eval_guide_formula(fmla: &str, vars: &BTreeMap<String, f64>) -> f64 {
    let tokens: Vec<&str> = fmla.split_whitespace().collect();
    if tokens.is_empty() {
        return 0.0;
    }
    let arg = |idx: usize| -> f64 { tokens.get(idx).map_or(0.0, |t| resolve_var_or_num(t, vars)) };

    match tokens[0] {
        "val" => arg(1),
        "*/" => {
            let x = arg(1);
            let y = arg(2);
            let z = arg(3);
            if z.abs() <= 1e-12 { 0.0 } else { (x * y) / z }
        }
        "+-" => arg(1) + arg(2) - arg(3),
        "+/" => {
            let x = arg(1);
            let y = arg(2);
            let z = arg(3);
            if z.abs() <= 1e-12 { 0.0 } else { (x + y) / z }
        }
        "?:" => {
            if arg(1) > 0.0 {
                arg(2)
            } else {
                arg(3)
            }
        }
        "abs" => arg(1).abs(),
        "at2" => {
            let x = arg(1);
            let y = arg(2);
            let rad = y.atan2(x);
            let mut deg60k = rad * (180.0 / PI) * ANGLE_UNITS_PER_DEG;
            if deg60k < 0.0 {
                deg60k += FULL_CIRCLE_UNITS;
            }
            deg60k
        }
        "cat2" => {
            let x = arg(1);
            let y = arg(2);
            let z = arg(3);
            x * z.atan2(y).cos()
        }
        "sat2" => {
            let x = arg(1);
            let y = arg(2);
            let z = arg(3);
            x * z.atan2(y).sin()
        }
        "cos" => {
            let x = arg(1);
            let ang_rad = (arg(2) / ANGLE_UNITS_PER_DEG) * (PI / 180.0);
            x * ang_rad.cos()
        }
        "sin" => {
            let x = arg(1);
            let ang_rad = (arg(2) / ANGLE_UNITS_PER_DEG) * (PI / 180.0);
            x * ang_rad.sin()
        }
        "tan" => {
            let x = arg(1);
            let ang_rad = (arg(2) / ANGLE_UNITS_PER_DEG) * (PI / 180.0);
            x * ang_rad.tan()
        }
        "sqrt" => arg(1).max(0.0).sqrt(),
        "max" => arg(1).max(arg(2)),
        "min" => arg(1).min(arg(2)),
        "mod" => {
            let x = arg(1);
            let y = arg(2);
            let z = arg(3);
            (x * x + y * y + z * z).sqrt()
        }
        "pin" => {
            let lo = arg(1);
            let val = arg(2);
            let hi = arg(3);
            if val < lo {
                lo
            } else if val > hi {
                hi
            } else {
                val
            }
        }
        other => resolve_var_or_num(other, vars),
    }
}

fn resolve_var_or_num(token: &str, vars: &BTreeMap<String, f64>) -> f64 {
    if let Some(&v) = vars.get(token) {
        return v;
    }
    token.parse::<f64>().unwrap_or(0.0)
}

/// Build positioned [`PathCommand`]s for a [`CustomGeometry`] inside `bounds`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn build_custom_geometry_paths(geom: &CustomGeometry, bounds: Rect) -> Vec<PathCommand> {
    let w = f64::from(bounds.width.max(1.0));
    let h = f64::from(bounds.height.max(1.0));
    let mut vars = builtin_shape_vars(w, h);

    for av in &geom.adjust_values {
        let val = eval_guide_formula(&av.fmla, &vars);
        vars.insert(av.name.clone(), val);
    }
    for gd in &geom.guides {
        let val = eval_guide_formula(&gd.fmla, &vars);
        vars.insert(gd.name.clone(), val);
    }

    let mut commands = Vec::new();
    for path in &geom.paths {
        let scale_x = if path.width > 0 {
            bounds.width / (path.width as f32)
        } else {
            1.0
        };
        let scale_y = if path.height > 0 {
            bounds.height / (path.height as f32)
        } else {
            1.0
        };

        let map_pt = |x_tok: &str, y_tok: &str, v: &BTreeMap<String, f64>| -> Point {
            let rx = resolve_var_or_num(x_tok, v) as f32;
            let ry = resolve_var_or_num(y_tok, v) as f32;
            Point::new(bounds.x + rx * scale_x, bounds.y + ry * scale_y)
        };

        let mut cur_pt = Point::new(bounds.x, bounds.y);
        for cmd in &path.commands {
            match cmd {
                CustomPathCmd::MoveTo { x, y } => {
                    cur_pt = map_pt(x, y, &vars);
                    commands.push(PathCommand::MoveTo(cur_pt));
                }
                CustomPathCmd::LineTo { x, y } => {
                    cur_pt = map_pt(x, y, &vars);
                    commands.push(PathCommand::LineTo(cur_pt));
                }
                CustomPathCmd::CubicBezTo { pt1, pt2, pt3 } => {
                    let ctrl1 = map_pt(&pt1.0, &pt1.1, &vars);
                    let ctrl2 = map_pt(&pt2.0, &pt2.1, &vars);
                    let to = map_pt(&pt3.0, &pt3.1, &vars);
                    cur_pt = to;
                    commands.push(PathCommand::CurveTo { ctrl1, ctrl2, to });
                }
                CustomPathCmd::QuadBezTo { pt1, pt2 } => {
                    let ctrl = map_pt(&pt1.0, &pt1.1, &vars);
                    let to = map_pt(&pt2.0, &pt2.1, &vars);
                    cur_pt = to;
                    commands.push(PathCommand::QuadTo { ctrl, to });
                }
                CustomPathCmd::ArcTo {
                    wr,
                    hr,
                    st_ang,
                    sw_ang,
                } => {
                    let rx = (resolve_var_or_num(wr, &vars) as f32) * scale_x;
                    let ry = (resolve_var_or_num(hr, &vars) as f32) * scale_y;
                    let start_deg =
                        (resolve_var_or_num(st_ang, &vars) / ANGLE_UNITS_PER_DEG) as f32;
                    let sweep_deg =
                        (resolve_var_or_num(sw_ang, &vars) / ANGLE_UNITS_PER_DEG) as f32;
                    append_arc_segments(&mut commands, &mut cur_pt, rx, ry, start_deg, sweep_deg);
                }
                CustomPathCmd::Close => {
                    commands.push(PathCommand::Close);
                }
            }
        }
    }

    commands
}

/// Build positioned [`PathCommand`]s for any of the 187 ECMA-376 preset shape types (`a:prstGeom`).
#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names
)]
pub fn build_preset_geometry_paths(
    preset: &str,
    bounds: Rect,
    adjusts: &[(String, f64)],
) -> Vec<PathCommand> {
    let x = bounds.x;
    let y = bounds.y;
    let w = bounds.width.max(1.0);
    let h = bounds.height.max(1.0);
    let ss = w.min(h);
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;

    let adj = |name: &str, default_val: f64| -> f32 {
        adjusts
            .iter()
            .find(|(k, _)| k == name)
            .map_or(default_val, |(_, v)| *v) as f32
    };

    match preset {
        "line" | "straightConnector1" => vec![
            PathCommand::MoveTo(Point::new(x, y)),
            PathCommand::LineTo(Point::new(x + w, y + h)),
        ],
        "lineInv" => vec![
            PathCommand::MoveTo(Point::new(x, y + h)),
            PathCommand::LineTo(Point::new(x + w, y)),
        ],
        "bentConnector2" => vec![
            PathCommand::MoveTo(Point::new(x, y)),
            PathCommand::LineTo(Point::new(x + w, y)),
            PathCommand::LineTo(Point::new(x + w, y + h)),
        ],
        "bentConnector3" | "bentConnector4" | "bentConnector5" => {
            let mid_x = x + w * (adj("adj1", 50_000.0) / 100_000.0).clamp(0.0, 1.0);
            vec![
                PathCommand::MoveTo(Point::new(x, y)),
                PathCommand::LineTo(Point::new(mid_x, y)),
                PathCommand::LineTo(Point::new(mid_x, y + h)),
                PathCommand::LineTo(Point::new(x + w, y + h)),
            ]
        }
        "curvedConnector2" | "curvedConnector3" | "curvedConnector4" | "curvedConnector5" => {
            let mid_x = x + w * (adj("adj1", 50_000.0) / 100_000.0).clamp(0.0, 1.0);
            vec![
                PathCommand::MoveTo(Point::new(x, y)),
                PathCommand::CurveTo {
                    ctrl1: Point::new(mid_x, y),
                    ctrl2: Point::new(mid_x, y + h),
                    to: Point::new(x + w, y + h),
                },
            ]
        }
        "rect"
        | "flowChartProcess"
        | "actionButtonBlank"
        | "actionButtonHome"
        | "actionButtonHelp"
        | "actionButtonInformation"
        | "actionButtonForwardNext"
        | "actionButtonBackPrevious"
        | "actionButtonEnd"
        | "actionButtonBeginning"
        | "actionButtonReturn"
        | "actionButtonDocument"
        | "actionButtonSound"
        | "actionButtonMovie" => rect_commands(x, y, w, h),

        "roundRect"
        | "flowChartAlternateProcess"
        | "round1Rect"
        | "round2SameRect"
        | "round2DiagRect" => {
            let ratio = (adj("adj", 16_667.0) / 100_000.0).clamp(0.0, 0.5);
            let r = ss * ratio;
            round_rect_commands(x, y, w, h, r)
        }
        "flowChartTerminator" => round_rect_commands(x, y, w, h, ss * 0.48),

        "snip1Rect"
        | "snip2SameRect"
        | "snip2DiagRect"
        | "snipRoundRect"
        | "flowChartPunchedCard" => {
            let s = ss * (adj("adj", 16_667.0) / 100_000.0).clamp(0.05, 0.45);
            polygon_commands(&[
                (x + s, y),
                (x + w - s, y),
                (x + w, y + s),
                (x + w, y + h),
                (x, y + h),
                (x, y + s),
            ])
        }
        "ellipse" | "flowChartConnector" | "flowChartSummingJunction" | "flowChartOr" => {
            ellipse_commands(cx, cy, w * 0.5, h * 0.5)
        }
        "donut" | "noSmoking" => {
            let mut cmds = ellipse_commands(cx, cy, w * 0.5, h * 0.5);
            let ratio = 0.5 - (adj("adj", 25_000.0) / 100_000.0).clamp(0.05, 0.45);
            cmds.extend(ellipse_commands(cx, cy, w * ratio, h * ratio));
            cmds
        }
        "triangle" | "flowChartExtract" => {
            let apex_x = x + w * (adj("adj", 50_000.0) / 100_000.0).clamp(0.0, 1.0);
            polygon_commands(&[(apex_x, y), (x + w, y + h), (x, y + h)])
        }
        "flowChartMerge" => polygon_commands(&[(x, y), (x + w, y), (cx, y + h)]),
        "flowChartCollate" => {
            let mut cmds = polygon_commands(&[(x, y), (x + w, y), (cx, cy)]);
            cmds.extend(polygon_commands(&[(cx, cy), (x + w, y + h), (x, y + h)]));
            cmds
        }
        "flowChartSort" => {
            let mut cmds = polygon_commands(&[(cx, y), (x + w, cy), (x, cy)]);
            cmds.extend(polygon_commands(&[(x, cy), (x + w, cy), (cx, y + h)]));
            cmds
        }
        "rtTriangle" => polygon_commands(&[(x, y), (x + w, y + h), (x, y + h)]),
        "diamond" | "flowChartDecision" => {
            polygon_commands(&[(cx, y), (x + w, cy), (cx, y + h), (x, cy)])
        }
        "parallelogram" | "flowChartInputOutput" => {
            let off = ss * (adj("adj", 25_000.0) / 100_000.0).clamp(0.0, 0.5);
            polygon_commands(&[(x + off, y), (x + w, y), (x + w - off, y + h), (x, y + h)])
        }
        "trapezoid" | "nonIsoscelesTrapezoid" => {
            let off = ss * (adj("adj", 25_000.0) / 100_000.0).clamp(0.0, 0.45);
            polygon_commands(&[(x + off, y), (x + w - off, y), (x + w, y + h), (x, y + h)])
        }
        "flowChartManualOperation" | "funnel" => {
            let off = w * 0.2;
            polygon_commands(&[(x, y), (x + w, y), (x + w - off, y + h), (x + off, y + h)])
        }
        "flowChartManualInput" => {
            polygon_commands(&[(x, y + h * 0.2), (x + w, y), (x + w, y + h), (x, y + h)])
        }
        "pentagon" => regular_polygon_commands(cx, cy, w * 0.5, h * 0.5, 5),
        "hexagon" | "flowChartPreparation" => {
            let off = ss * (adj("adj", 25_000.0) / 100_000.0).clamp(0.1, 0.45);
            polygon_commands(&[
                (x + off, y),
                (x + w - off, y),
                (x + w, cy),
                (x + w - off, y + h),
                (x + off, y + h),
                (x, cy),
            ])
        }
        "heptagon" => regular_polygon_commands(cx, cy, w * 0.5, h * 0.5, 7),
        "octagon" => regular_polygon_commands(cx, cy, w * 0.5, h * 0.5, 8),
        "decagon" => regular_polygon_commands(cx, cy, w * 0.5, h * 0.5, 10),
        "dodecagon" => regular_polygon_commands(cx, cy, w * 0.5, h * 0.5, 12),

        "star4" => star_commands(cx, cy, w * 0.5, h * 0.5, 4, 0.35),
        "star5" | "chartStar" => star_commands(cx, cy, w * 0.5, h * 0.5, 5, 0.382),
        "star6" => star_commands(cx, cy, w * 0.5, h * 0.5, 6, 0.5),
        "star7" => star_commands(cx, cy, w * 0.5, h * 0.5, 7, 0.5),
        "star8" => star_commands(cx, cy, w * 0.5, h * 0.5, 8, 0.55),
        "star10" => star_commands(cx, cy, w * 0.5, h * 0.5, 10, 0.6),
        "star12" => star_commands(cx, cy, w * 0.5, h * 0.5, 12, 0.65),
        "star16" | "irregularSeal1" => star_commands(cx, cy, w * 0.5, h * 0.5, 16, 0.7),
        "star24" | "irregularSeal2" => star_commands(cx, cy, w * 0.5, h * 0.5, 24, 0.75),
        "star32" => star_commands(cx, cy, w * 0.5, h * 0.5, 32, 0.8),

        "gear6" => star_commands(cx, cy, w * 0.5, h * 0.5, 6, 0.72),
        "gear9" => star_commands(cx, cy, w * 0.5, h * 0.5, 9, 0.75),

        "rightArrow" | "stripedRightArrow" | "notchedRightArrow" => {
            let head_w = (ss * (adj("adj2", 50_000.0) / 100_000.0)).min(w * 0.6);
            let shaft_half = h * (adj("adj1", 50_000.0) / 100_000.0).clamp(0.1, 0.9) * 0.5;
            let notch = if preset == "notchedRightArrow" {
                head_w * 0.4
            } else {
                0.0
            };
            polygon_commands(&[
                (x, cy - shaft_half),
                (x + w - head_w, cy - shaft_half),
                (x + w - head_w, y),
                (x + w, cy),
                (x + w - head_w, y + h),
                (x + w - head_w, cy + shaft_half),
                (x, cy + shaft_half),
                (x + notch, cy),
            ])
        }
        "leftArrow" => {
            let head_w = (ss * 0.5).min(w * 0.6);
            let shaft_half = h * 0.25;
            polygon_commands(&[
                (x, cy),
                (x + head_w, y),
                (x + head_w, cy - shaft_half),
                (x + w, cy - shaft_half),
                (x + w, cy + shaft_half),
                (x + head_w, cy + shaft_half),
                (x + head_w, y + h),
            ])
        }
        "upArrow" => {
            let head_h = (ss * 0.5).min(h * 0.6);
            let shaft_half = w * 0.25;
            polygon_commands(&[
                (cx, y),
                (x + w, y + head_h),
                (cx + shaft_half, y + head_h),
                (cx + shaft_half, y + h),
                (cx - shaft_half, y + h),
                (cx - shaft_half, y + head_h),
                (x, y + head_h),
            ])
        }
        "downArrow" | "flowChartOffpageConnector" => {
            let head_h = (ss * 0.45).min(h * 0.55);
            let shaft_half = if preset == "flowChartOffpageConnector" {
                w * 0.5
            } else {
                w * 0.25
            };
            polygon_commands(&[
                (cx - shaft_half, y),
                (cx + shaft_half, y),
                (cx + shaft_half, y + h - head_h),
                (x + w, y + h - head_h),
                (cx, y + h),
                (x, y + h - head_h),
                (cx - shaft_half, y + h - head_h),
            ])
        }
        "leftRightArrow"
        | "upDownArrow"
        | "quadArrow"
        | "leftUpArrow"
        | "leftRightUpArrow"
        | "bentUpArrow"
        | "bentArrow"
        | "uturnArrow"
        | "circularArrow"
        | "leftCircularArrow"
        | "leftRightCircularArrow"
        | "curvedRightArrow"
        | "curvedLeftArrow"
        | "curvedUpArrow"
        | "curvedDownArrow"
        | "swooshArrow" => {
            let head_w = (ss * 0.35).min(w * 0.35);
            let shaft_half = h * 0.25;
            polygon_commands(&[
                (x, cy),
                (x + head_w, y),
                (x + head_w, cy - shaft_half),
                (x + w - head_w, cy - shaft_half),
                (x + w - head_w, y),
                (x + w, cy),
                (x + w - head_w, y + h),
                (x + w - head_w, cy + shaft_half),
                (x + head_w, cy + shaft_half),
                (x + head_w, y + h),
            ])
        }
        "homePlate" => {
            let tip = (ss * 0.4).min(w * 0.5);
            polygon_commands(&[
                (x, y),
                (x + w - tip, y),
                (x + w, cy),
                (x + w - tip, y + h),
                (x, y + h),
            ])
        }
        "chevron" => {
            let tip = (ss * 0.35).min(w * 0.45);
            polygon_commands(&[
                (x, y),
                (x + w - tip, y),
                (x + w, cy),
                (x + w - tip, y + h),
                (x, y + h),
                (x + tip, cy),
            ])
        }
        "plus" | "mathPlus" | "chartPlus" => {
            let arm = ss * 0.28;
            polygon_commands(&[
                (cx - arm, y),
                (cx + arm, y),
                (cx + arm, cy - arm),
                (x + w, cy - arm),
                (x + w, cy + arm),
                (cx + arm, cy + arm),
                (cx + arm, y + h),
                (cx - arm, y + h),
                (cx - arm, cy + arm),
                (x, cy + arm),
                (x, cy - arm),
                (cx - arm, cy - arm),
            ])
        }
        "mathMinus" => rect_commands(x + w * 0.15, cy - h * 0.12, w * 0.7, h * 0.24),
        "mathEqual" | "mathNotEqual" => {
            let mut cmds = rect_commands(x + w * 0.15, cy - h * 0.25, w * 0.7, h * 0.15);
            cmds.extend(rect_commands(
                x + w * 0.15,
                cy + h * 0.10,
                w * 0.7,
                h * 0.15,
            ));
            cmds
        }
        "mathMultiply" | "mathDivide" | "chartX" => {
            let d = ss * 0.15;
            polygon_commands(&[
                (x + d, y),
                (cx, cy - d),
                (x + w - d, y),
                (x + w, y + d),
                (cx + d, cy),
                (x + w, y + h - d),
                (x + w - d, y + h),
                (cx, cy + d),
                (x + d, y + h),
                (x, y + h - d),
                (cx - d, cy),
                (x, y + d),
            ])
        }
        "foldedCorner"
        | "flowChartDocument"
        | "flowChartMultidocument"
        | "flowChartPunchedTape"
        | "wave"
        | "doubleWave" => {
            let fold = ss * 0.2;
            polygon_commands(&[
                (x, y),
                (x + w - fold, y),
                (x + w, y + fold),
                (x + w, y + h),
                (x, y + h),
            ])
        }
        "heart" => vec![
            PathCommand::MoveTo(Point::new(cx, y + h * 0.28)),
            PathCommand::CurveTo {
                ctrl1: Point::new(cx, y),
                ctrl2: Point::new(x, y),
                to: Point::new(x, y + h * 0.35),
            },
            PathCommand::CurveTo {
                ctrl1: Point::new(x, y + h * 0.65),
                ctrl2: Point::new(cx, y + h * 0.8),
                to: Point::new(cx, y + h),
            },
            PathCommand::CurveTo {
                ctrl1: Point::new(cx, y + h * 0.8),
                ctrl2: Point::new(x + w, y + h * 0.65),
                to: Point::new(x + w, y + h * 0.35),
            },
            PathCommand::CurveTo {
                ctrl1: Point::new(x + w, y),
                ctrl2: Point::new(cx, y),
                to: Point::new(cx, y + h * 0.28),
            },
            PathCommand::Close,
        ],
        "lightningBolt" => polygon_commands(&[
            (x + w * 0.4, y),
            (x, y + h * 0.55),
            (x + w * 0.45, y + h * 0.55),
            (x + w * 0.3, y + h),
            (x + w, y + h * 0.4),
            (x + w * 0.55, y + h * 0.4),
        ]),
        "wedgeRectCallout"
        | "wedgeRoundRectCallout"
        | "wedgeEllipseCallout"
        | "cloudCallout"
        | "callout1"
        | "callout2"
        | "callout3"
        | "accentCallout1"
        | "accentCallout2"
        | "accentCallout3"
        | "borderCallout1"
        | "borderCallout2"
        | "borderCallout3"
        | "accentBorderCallout1"
        | "accentBorderCallout2"
        | "accentBorderCallout3"
        | "leftArrowCallout"
        | "rightArrowCallout"
        | "upArrowCallout"
        | "downArrowCallout"
        | "leftRightArrowCallout"
        | "upDownArrowCallout"
        | "quadArrowCallout" => polygon_commands(&[
            (x, y),
            (x + w, y),
            (x + w, y + h * 0.8),
            (x + w * 0.45, y + h * 0.8),
            (x + w * 0.25, y + h),
            (x + w * 0.30, y + h * 0.8),
            (x, y + h * 0.8),
        ]),
        "leftBracket" | "rightBracket" | "bracketPair" | "leftBrace" | "rightBrace"
        | "bracePair" => {
            let mut cmds = Vec::new();
            let arm = (w * 0.25).min(12.0);
            if !preset.starts_with("right") {
                cmds.push(PathCommand::MoveTo(Point::new(x + arm, y)));
                cmds.push(PathCommand::LineTo(Point::new(x, y)));
                cmds.push(PathCommand::LineTo(Point::new(x, y + h)));
                cmds.push(PathCommand::LineTo(Point::new(x + arm, y + h)));
            }
            if !preset.starts_with("left") {
                cmds.push(PathCommand::MoveTo(Point::new(x + w - arm, y)));
                cmds.push(PathCommand::LineTo(Point::new(x + w, y)));
                cmds.push(PathCommand::LineTo(Point::new(x + w, y + h)));
                cmds.push(PathCommand::LineTo(Point::new(x + w - arm, y + h)));
            }
            cmds
        }
        "frame" | "bevel" | "flowChartPredefinedProcess" | "flowChartInternalStorage" => {
            let mut cmds = rect_commands(x, y, w, h);
            let inset = ss * 0.12;
            cmds.extend(rect_commands(
                x + inset,
                y + inset,
                (w - 2.0 * inset).max(2.0),
                (h - 2.0 * inset).max(2.0),
            ));
            cmds
        }
        _ => {
            // Remaining presets (clouds, ribbons, scrolls, arcs, cubes, cans, cylinders, plaques, tabs)
            round_rect_commands(x, y, w, h, ss * 0.12)
        }
    }
}

fn rect_commands(x: f32, y: f32, w: f32, h: f32) -> Vec<PathCommand> {
    vec![
        PathCommand::MoveTo(Point::new(x, y)),
        PathCommand::LineTo(Point::new(x + w, y)),
        PathCommand::LineTo(Point::new(x + w, y + h)),
        PathCommand::LineTo(Point::new(x, y + h)),
        PathCommand::Close,
    ]
}

#[allow(clippy::many_single_char_names)]
fn round_rect_commands(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<PathCommand> {
    let r = r.min(w * 0.5).min(h * 0.5);
    if r <= 0.5 {
        return rect_commands(x, y, w, h);
    }
    let k = 0.552_284_8 * r;
    vec![
        PathCommand::MoveTo(Point::new(x + r, y)),
        PathCommand::LineTo(Point::new(x + w - r, y)),
        PathCommand::CurveTo {
            ctrl1: Point::new(x + w - r + k, y),
            ctrl2: Point::new(x + w, y + r - k),
            to: Point::new(x + w, y + r),
        },
        PathCommand::LineTo(Point::new(x + w, y + h - r)),
        PathCommand::CurveTo {
            ctrl1: Point::new(x + w, y + h - r + k),
            ctrl2: Point::new(x + w - r + k, y + h),
            to: Point::new(x + w - r, y + h),
        },
        PathCommand::LineTo(Point::new(x + r, y + h)),
        PathCommand::CurveTo {
            ctrl1: Point::new(x + r - k, y + h),
            ctrl2: Point::new(x, y + h - r + k),
            to: Point::new(x, y + h - r),
        },
        PathCommand::LineTo(Point::new(x, y + r)),
        PathCommand::CurveTo {
            ctrl1: Point::new(x, y + r - k),
            ctrl2: Point::new(x + r - k, y),
            to: Point::new(x + r, y),
        },
        PathCommand::Close,
    ]
}

fn ellipse_commands(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<PathCommand> {
    let k = 0.552_284_8;
    let kx = rx * k;
    let ky = ry * k;
    vec![
        PathCommand::MoveTo(Point::new(cx + rx, cy)),
        PathCommand::CurveTo {
            ctrl1: Point::new(cx + rx, cy + ky),
            ctrl2: Point::new(cx + kx, cy + ry),
            to: Point::new(cx, cy + ry),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx - kx, cy + ry),
            ctrl2: Point::new(cx - rx, cy + ky),
            to: Point::new(cx - rx, cy),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx - rx, cy - ky),
            ctrl2: Point::new(cx - kx, cy - ry),
            to: Point::new(cx, cy - ry),
        },
        PathCommand::CurveTo {
            ctrl1: Point::new(cx + kx, cy - ry),
            ctrl2: Point::new(cx + rx, cy - ky),
            to: Point::new(cx + rx, cy),
        },
        PathCommand::Close,
    ]
}

fn polygon_commands(pts: &[(f32, f32)]) -> Vec<PathCommand> {
    let mut cmds = Vec::with_capacity(pts.len() + 1);
    for (i, &(px, py)) in pts.iter().enumerate() {
        if i == 0 {
            cmds.push(PathCommand::MoveTo(Point::new(px, py)));
        } else {
            cmds.push(PathCommand::LineTo(Point::new(px, py)));
        }
    }
    cmds.push(PathCommand::Close);
    cmds
}

#[allow(clippy::cast_precision_loss)]
fn regular_polygon_commands(cx: f32, cy: f32, rx: f32, ry: f32, sides: usize) -> Vec<PathCommand> {
    let mut pts = Vec::with_capacity(sides);
    let step = std::f32::consts::TAU / (sides as f32);
    let start_ang = -std::f32::consts::FRAC_PI_2;
    for i in 0..sides {
        let ang = start_ang + (i as f32) * step;
        pts.push((cx + rx * ang.cos(), cy + ry * ang.sin()));
    }
    polygon_commands(&pts)
}

#[allow(clippy::cast_precision_loss)]
fn star_commands(
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    points: usize,
    inner_ratio: f32,
) -> Vec<PathCommand> {
    let total_verts = points * 2;
    let mut pts = Vec::with_capacity(total_verts);
    let step = std::f32::consts::TAU / (total_verts as f32);
    let start_ang = -std::f32::consts::FRAC_PI_2;
    for i in 0..total_verts {
        let ang = start_ang + (i as f32) * step;
        let r_scale = if i % 2 == 0 { 1.0 } else { inner_ratio };
        pts.push((cx + rx * r_scale * ang.cos(), cy + ry * r_scale * ang.sin()));
    }
    polygon_commands(&pts)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::similar_names
)]
fn append_arc_segments(
    cmds: &mut Vec<PathCommand>,
    cur_pt: &mut Point,
    rx: f32,
    ry: f32,
    st_deg: f32,
    sw_deg: f32,
) {
    if rx.abs() <= 0.01 || ry.abs() <= 0.01 || sw_deg.abs() <= 0.01 {
        return;
    }
    let st_rad = st_deg.to_radians();
    let cx = cur_pt.x - rx * st_rad.cos();
    let cy = cur_pt.y - ry * st_rad.sin();
    let steps = ((sw_deg.abs() / 15.0).ceil() as usize).clamp(2, 24);
    for s in 1..=steps {
        let t = (s as f32) / (steps as f32);
        let ang = (st_deg + sw_deg * t).to_radians();
        let next = Point::new(cx + rx * ang.cos(), cy + ry * ang.sin());
        cmds.push(PathCommand::LineTo(next));
        *cur_pt = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_187_preset_shapes_produce_valid_finite_paths() {
        assert_eq!(ALL_PRESET_SHAPES.len(), 187);
        let bounds = Rect::new(10.0, 20.0, 120.0, 80.0);
        for &preset in &ALL_PRESET_SHAPES {
            assert!(is_known_preset_shape(preset));
            let cmds = build_preset_geometry_paths(preset, bounds, &[]);
            assert!(
                cmds.len() >= 2,
                "Preset {preset} produced fewer than 2 path commands"
            );
            for cmd in &cmds {
                match cmd {
                    PathCommand::MoveTo(p) | PathCommand::LineTo(p) => {
                        assert!(p.x.is_finite() && p.y.is_finite());
                    }
                    PathCommand::QuadTo { ctrl, to } => {
                        assert!(ctrl.x.is_finite() && to.x.is_finite());
                    }
                    PathCommand::CurveTo { ctrl1, ctrl2, to } => {
                        assert!(ctrl1.x.is_finite() && ctrl2.x.is_finite() && to.x.is_finite());
                    }
                    PathCommand::Close => {}
                }
            }
        }
    }

    #[test]
    fn evaluates_drawingml_guide_formulas_and_custom_geometry() {
        let mut vars = builtin_shape_vars(200.0, 100.0);
        vars.insert("adj".to_string(), 25_000.0);
        assert!((eval_guide_formula("*/ ss adj 100000", &vars) - 25.0).abs() < 1e-6);
        assert!((eval_guide_formula("+- r 0 hc", &vars) - 100.0).abs() < 1e-6);
        assert!((eval_guide_formula("pin 10 5 50", &vars) - 10.0).abs() < 1e-6);
        assert!((eval_guide_formula("?: 1 42 99", &vars) - 42.0).abs() < 1e-6);
        assert!((eval_guide_formula("mod 3 4 0", &vars) - 5.0).abs() < 1e-6);

        let cust = CustomGeometry {
            adjust_values: vec![GuideDef {
                name: "adj1".to_string(),
                fmla: "val 50000".to_string(),
            }],
            guides: vec![GuideDef {
                name: "mx".to_string(),
                fmla: "*/ w adj1 100000".to_string(),
            }],
            paths: vec![CustomPath {
                width: 0,
                height: 0,
                commands: vec![
                    CustomPathCmd::MoveTo {
                        x: "mx".to_string(),
                        y: "0".to_string(),
                    },
                    CustomPathCmd::LineTo {
                        x: "w".to_string(),
                        y: "h".to_string(),
                    },
                    CustomPathCmd::LineTo {
                        x: "0".to_string(),
                        y: "h".to_string(),
                    },
                    CustomPathCmd::Close,
                ],
            }],
        };
        let cmds = build_custom_geometry_paths(&cust, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[0], PathCommand::MoveTo(Point::new(100.0, 0.0)));
    }
}
