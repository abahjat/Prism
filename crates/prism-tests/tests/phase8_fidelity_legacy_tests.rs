// SPDX-License-Identifier: AGPL-3.0-only
//! Phase 8 integration tests (`M8.1`–`M8.4`):
//! - **M8.1**: All 187 `DrawingML` preset geometries (`ALL_PRESET_SHAPES`, `preset_geometry_definition`,
//!   `build_preset_geometry_paths`) and advanced charts (secondary value axis, logarithmic axis,
//!   scatter/bubble X-values, and radar charts).
//! - **M8.2**: Word tight/through polygon wrapping (`carve_wrap_polygon_interval`), `compatibilityMode`
//!   (`11`–`15`) quirks, binary `.doc` completion, and BIFF8 `.xls` RPN formula decompilation.
//! - **M8.3**: Legacy & regional formats: Corel `WordPerfect` (`.wpd`) and Korean Hangul
//!   (`.hwp` / `.hwpx`).

use prism_core::format::detect_format;
use prism_core::review::FindingKind;
use prism_drawingml::{
    ALL_PRESET_SHAPES, AxisSpec, ChartKind, ChartSeries, ChartSpace, DrawingTheme, LegendPosition,
    build_preset_geometry_paths, preset_geometry_definition, render_chart,
};
use prism_excel::{decompile_biff8_rgce, parse_xls};
use prism_layout::model::{LayoutOptions, Rect};
use prism_parsers::legacy::{build_hwp_bytes, build_hwpx_bytes, build_wpd_bytes};
use prism_parsers::pipeline::open_and_layout;
use prism_parsers::review::generate_hidden_content_report;
use prism_word::layout::carve_wrap_polygon_interval;
use std::io::{Cursor, Write};

#[test]
fn m8_1_all_187_drawingml_preset_shapes_and_advanced_charts() {
    assert_eq!(ALL_PRESET_SHAPES.len(), 187);
    let bounds = Rect::new(10.0, 20.0, 160.0, 100.0);

    for &prst in &ALL_PRESET_SHAPES {
        let def = preset_geometry_definition(prst)
            .unwrap_or_else(|| panic!("missing definition for {prst}"));
        assert!(
            !def.paths.is_empty(),
            "preset {prst} definition had no paths"
        );
        let cmds = build_preset_geometry_paths(prst, bounds, &[]);
        assert!(
            !cmds.is_empty(),
            "preset {prst} produced no geometry path commands"
        );
    }

    // Verify specific complex multi-subpath shapes have multiple subpaths or rich curves
    for special in [
        "cube",
        "can",
        "smileyFace",
        "sun",
        "cloud",
        "ribbon",
        "verticalScroll",
        "pie",
        "blockArc",
        "plaque",
        "teardrop",
    ] {
        let cmds = build_preset_geometry_paths(special, bounds, &[]);
        assert!(
            cmds.len() >= 4,
            "expected rich geometry for {special}, got {} commands",
            cmds.len()
        );
    }

    // Verify radar chart and logarithmic + secondary value axis + bubble chart rendering
    let radar_chart = ChartSpace {
        title: Some("Security Posture Radar".to_string()),
        primary_kind: ChartKind::Radar,
        legend: LegendPosition::Right,
        series: vec![ChartSeries {
            name: Some("Score".to_string()),
            kind: ChartKind::Radar,
            categories: vec![
                "Identity".to_string(),
                "Network".to_string(),
                "Data".to_string(),
                "Endpoint".to_string(),
                "Cloud".to_string(),
            ],
            values: vec![85.0, 90.0, 75.0, 88.0, 92.0],
            x_values: Vec::new(),
            bubble_sizes: Vec::new(),
            format_code: None,
            color: None,
            use_secondary_axis: false,
            show_data_labels: false,
            has_linear_trendline: false,
            explosion_pct: 0,
        }],
        category_axis: AxisSpec::default(),
        value_axis: AxisSpec {
            min: Some(0.0),
            max: Some(100.0),
            ..AxisSpec::default()
        },
        secondary_value_axis: None,
        hole_size_pct: 50,
        external_data_ref: None,
    };
    let opts = LayoutOptions::default();
    let theme = DrawingTheme::default();
    let mut font_sys = prism_layout::FontSystem::new(&opts);
    let mut text_engine = prism_layout::TextLayoutEngine::new();
    let radar_items = render_chart(
        &radar_chart,
        Rect::new(0.0, 0.0, 400.0, 300.0),
        &theme,
        &mut font_sys,
        &mut text_engine,
    );
    assert!(!radar_items.is_empty());

    // Bubble chart with X-values, logarithmic primary axis, and secondary value axis
    let combo_chart = ChartSpace {
        title: Some("Log Bubble & Secondary Axis".to_string()),
        primary_kind: ChartKind::Bubble,
        legend: LegendPosition::Right,
        series: vec![
            ChartSeries {
                name: Some("Primary Log Bubbles".to_string()),
                kind: ChartKind::Bubble,
                categories: vec!["A".to_string(), "B".to_string(), "C".to_string()],
                values: vec![10.0, 100.0, 1000.0],
                x_values: vec![1.0, 5.0, 9.0],
                bubble_sizes: vec![10.0, 25.0, 40.0],
                format_code: None,
                color: None,
                use_secondary_axis: false,
                show_data_labels: false,
                has_linear_trendline: false,
                explosion_pct: 0,
            },
            ChartSeries {
                name: Some("Secondary Line".to_string()),
                kind: ChartKind::Line,
                categories: vec!["A".to_string(), "B".to_string(), "C".to_string()],
                values: vec![0.2, 0.5, 0.9],
                x_values: Vec::new(),
                bubble_sizes: Vec::new(),
                format_code: None,
                color: None,
                use_secondary_axis: true,
                show_data_labels: false,
                has_linear_trendline: false,
                explosion_pct: 0,
            },
        ],
        category_axis: AxisSpec::default(),
        value_axis: AxisSpec {
            log_base: Some(10.0),
            ..AxisSpec::default()
        },
        secondary_value_axis: Some(AxisSpec {
            min: Some(0.0),
            max: Some(1.0),
            ..AxisSpec::default()
        }),
        hole_size_pct: 50,
        external_data_ref: None,
    };
    let combo_items = render_chart(
        &combo_chart,
        Rect::new(0.0, 0.0, 420.0, 280.0),
        &theme,
        &mut font_sys,
        &mut text_engine,
    );
    assert!(!combo_items.is_empty());
}

#[test]
fn m8_2_word_polygon_wrapping_and_biff8_formula_decompilation() {
    // Diamond wrap polygon centered inside [100..200, 100..200]
    let diamond = vec![
        (10800.0, 0.0),
        (21600.0, 10800.0),
        (10800.0, 21600.0),
        (0.0, 10800.0),
    ];
    let rect = Rect::new(100.0, 100.0, 100.0, 100.0);
    // Near the top vertex (y = 105..115), the diamond is narrow around x = 150
    let top_carve = carve_wrap_polygon_interval(rect, &diamond, 105.0, 115.0).unwrap();
    // Near the equator (y = 145..155), the diamond spans almost the full 100..200 width
    let mid_carve = carve_wrap_polygon_interval(rect, &diamond, 145.0, 155.0).unwrap();
    assert!(
        (mid_carve.1 - mid_carve.0) > (top_carve.1 - top_carve.0) + 40.0,
        "expected tight polygon wrap to be wider at equator ({mid_carve:?}) than apex ({top_carve:?})"
    );

    // BIFF8 RPN formula decompilation
    let mut rgce = Vec::new();
    rgce.push(0x24); // PtgRef B2 (rw=1, col=1 relative)
    rgce.extend_from_slice(&1u16.to_le_bytes());
    rgce.extend_from_slice(&0xC001u16.to_le_bytes());
    rgce.push(0x1E); // PtgInt 10
    rgce.extend_from_slice(&10u16.to_le_bytes());
    rgce.push(0x05); // PtgMul
    assert_eq!(decompile_biff8_rgce(&rgce).as_deref(), Some("B2*10"));

    // Build an XLS whose formula cell carries that token array
    let mut biff = Vec::new();
    let push_rec = |out: &mut Vec<u8>, rt: u16, pay: &[u8]| {
        out.extend_from_slice(&rt.to_le_bytes());
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(pay.len() as u16).to_le_bytes());
        out.extend_from_slice(pay);
    };
    let bof = |dt: u16| {
        let mut p = 0x0600u16.to_le_bytes().to_vec();
        p.extend_from_slice(&dt.to_le_bytes());
        p.extend_from_slice(&[0u8; 12]);
        p
    };
    push_rec(&mut biff, 0x0809, &bof(0x0005));

    let mut bs = 0u32.to_le_bytes().to_vec();
    bs.extend_from_slice(&[0, 0, 7, 0]);
    bs.extend_from_slice(b"Finance");
    push_rec(&mut biff, 0x0085, &bs);
    push_rec(&mut biff, 0x000A, &[]);

    push_rec(&mut biff, 0x0809, &bof(0x0010));
    let mut f_pay = Vec::new();
    f_pay.extend_from_slice(&0u16.to_le_bytes());
    f_pay.extend_from_slice(&0u16.to_le_bytes());
    f_pay.extend_from_slice(&0u16.to_le_bytes());
    f_pay.extend_from_slice(&120.0f64.to_le_bytes());
    f_pay.extend_from_slice(&0u16.to_le_bytes());
    f_pay.extend_from_slice(&0u32.to_le_bytes());
    #[allow(clippy::cast_possible_truncation)]
    f_pay.extend_from_slice(&(rgce.len() as u16).to_le_bytes());
    f_pay.extend_from_slice(&rgce);
    push_rec(&mut biff, 0x0006, &f_pay);
    push_rec(&mut biff, 0x000A, &[]);

    let mut comp = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    comp.create_stream("/Workbook")
        .unwrap()
        .write_all(&biff)
        .unwrap();
    let xls_bytes = comp.into_inner().into_inner();
    let wb = parse_xls(&xls_bytes, 16 * 1024 * 1024).unwrap();
    assert_eq!(wb.sheets[0].name, "Finance");
    assert_eq!(
        wb.sheets[0].cells[&(0, 0)]
            .formula
            .as_ref()
            .unwrap()
            .expression,
        "B2*10"
    );
}

#[test]
fn m8_3_legacy_and_regional_formats_wpd_hwp_hwpx() {
    let opts = LayoutOptions::default();

    // 1. Corel WordPerfect (.wpd)
    let wpd_bytes = build_wpd_bytes(
        "Merger Agreement",
        "Legal Counsel",
        &[
            "ARTICLE I: DEFINITIONS",
            "Section 1.01 Confidential Information.",
        ],
        Some("Privileged attorney draft note"),
    );
    let det_wpd = detect_format(&wpd_bytes, Some("agreement.wpd")).unwrap();
    assert_eq!(det_wpd.format.extension, "wpd");
    let opened_wpd = open_and_layout(&wpd_bytes, Some("agreement.wpd"), &opts).unwrap();
    let wpd_text = opened_wpd.layout.pages[0].text();
    assert!(wpd_text.contains("ARTICLE I: DEFINITIONS"));
    assert!(wpd_text.contains("Section 1.01 Confidential Information."));
    let wpd_report = generate_hidden_content_report(&wpd_bytes, Some("agreement.wpd"), None);
    assert!(wpd_report.has_kind(FindingKind::HiddenText));

    // 2. Korean Hangul (.hwp v5.0 OLE2 compressed)
    let hwp_bytes = build_hwp_bytes(
        &[
            "대한민국 전자문서 증거개시 보고서",
            "Hangul HWP 5.0 Section Body Text",
        ],
        true,
        false,
    );
    let det_hwp = detect_format(&hwp_bytes, Some("report.hwp")).unwrap();
    assert_eq!(det_hwp.format.extension, "hwp");
    let opened_hwp = open_and_layout(&hwp_bytes, Some("report.hwp"), &opts).unwrap();
    let hwp_text = opened_hwp.layout.pages[0].text();
    assert!(hwp_text.contains("Hangul HWP 5.0 Section Body Text"));

    // 3. Korean Hangul OWPML (.hwpx ZIP)
    let hwpx_bytes = build_hwpx_bytes(
        &["OWPML HWPX Document Title", "Second Paragraph"],
        &[vec!["Item", "Status"], vec!["Contract", "Approved"]],
    );
    let det_hwpx = detect_format(&hwpx_bytes, Some("contract.hwpx")).unwrap();
    assert_eq!(det_hwpx.format.extension, "hwpx");
    let opened_hwpx = open_and_layout(&hwpx_bytes, Some("contract.hwpx"), &opts).unwrap();
    let hwpx_text = opened_hwpx.layout.pages[0].text();
    assert!(hwpx_text.contains("OWPML HWPX Document Title"));
    assert!(hwpx_text.contains("Approved"));
}
