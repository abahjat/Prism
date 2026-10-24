# Native Charts Architecture (`prism-drawingml::chart`)

## Overview

[`prism-drawingml::chart`](../../crates/prism-drawingml/src/chart.rs) parses and renders both standard OOXML `DrawingML` charts (`c:chartSpace`, ECMA-376 §21.2) and Office 2016+ Extended Charts (`cx:chartSpace`, `http://schemas.microsoft.com/office/drawing/2014/chartex`) directly into vector [`prism_layout::Item`](../../crates/prism-layout/src/model.rs) primitives (`PathItem` + shaped `GlyphRun`).

The same chart engine is shared across all three Office families:
- **Word (`prism-word`)**: `<c:chart r:id="..."/>` inside `wp:inline` / `wp:anchor` frames (`word/charts/chart*.xml`).
- **PowerPoint (`prism-ppt`)**: `<p:graphicFrame>` chart shapes on slides (`ppt/charts/chart*.xml` and `ppt/charts/chartEx*.xml`).
- **Excel (`prism-excel`)**: worksheet drawings (`xl/drawings/drawing*.xml` → `xl/charts/chart*.xml` / `chartEx*.xml`) and standalone C ABI rendering via `prism_workbook_render_object`.

## Cached-Series-Only Principle (ADR-0009)

For forensic reproducibility and offline safety, chart rendering reads **only** the cached series values embedded inside the chart XML part:
- `c:numRef/c:numCache` and `c:numLit` for numeric series points and `<c:formatCode>` number formatting.
- `c:strRef/c:strCache`, `c:multiLvlStrRef/c:multiLvlStrCache`, and `c:strLit` for category labels and series titles.
- `cx:chartData/cx:data` (`cx:strDim` and `cx:numDim`) for Office 2016+ extended charts.

Prism **never** opens external workbooks referenced by `<c:externalData r:id="..."/>` and never evaluates worksheet formula references (`Sheet1!$A$1:$B$10`). If a chart contains series definitions with empty caches, `ChartSpace::missing_cache_series_count` is incremented and a `[Chart data cache not stored in file]` notice is rendered inside the plot area.

## Supported Chart Types

| Family | XML Element / Layout ID | Rendering Behavior |
|---|---|---|
| Bar / Column | `c:barChart`, `c:bar3DChart` | Clustered (`clustered`), stacked (`stacked`), and 100% stacked (`percentStacked`) vertical columns (`barDir="col"`) and horizontal bars (`barDir="bar"`). |
| Line / Area | `c:lineChart`, `c:line3DChart`, `c:areaChart`, `c:area3DChart` | Polyline series strokes, point markers, and semi-transparent filled polygons beneath area series. |
| Pie / Doughnut | `c:pieChart`, `c:pie3DChart`, `c:doughnutChart` | Proportional circular/annular SVG arc wedges with per-point theme/accent palette colors and legend labels. |
| Scatter / Bubble | `c:scatterChart`, `c:bubbleChart` | `(xVal, yVal)` coordinate plotting with scaled bubble radii (`bubbleSize`). |
| Radar | `c:radarChart` | Concentric spider-web grid polygons and closed radial series paths. |
| Combo | Multiple chart elements in `c:plotArea` | Overlaid bar + line / secondary-axis series within a unified plot rectangle. |
| Extended (`chartex`) | `cx:series layoutId="waterfall" \| "treemap" \| "sunburst" \| "histogram" \| "boxWhisker" \| "funnel"` | Running-total waterfall floating bars, proportional treemap tiles, and binned distribution columns. |
