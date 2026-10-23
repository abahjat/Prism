# Excel Number Format Compatibility (`prism-numfmt`)

[`prism-numfmt`](../crates/prism-numfmt/src/lib.rs) implements a pure-Rust, locale-independent ECMA-376 (§18.8.30) number and date/time formatting engine so that cached numeric values in `.xlsx`, `.xls`, and chart caches render identically across Windows, Linux, and macOS.

## Supported Format Features

| Feature | Syntax / IDs | Compatibility Notes |
|---|---|---|
| Built-in format IDs | `numFmtId = 0..=49` | Full table per ECMA-376 §18.8.30 (`0` = `General`, `1..=4` fixed/comma, `5..=8` currency, `9..=10` percent, `11` scientific, `12..=13` fraction, `14..=22` date/time, `37..=40` accounting, `44` accounting currency, `45..=47` elapsed time, `48` engineering scientific, `49` `@` text). |
| Multi-section strings | `positive;negative;zero;text` | Up to 4 `;`-separated sections. When 1 section is present it applies to all numbers; when 2 are present, section 1 is `>= 0` and section 2 is `< 0`; when 3+ are present, section 3 formats `0.0`. |
| Conditional brackets | `[<100]`, `[>=1000]`, `[=0]`, `[<>0]` | Up to 2 conditional predicates override the default positive/negative/zero section selection. |
| Color tokens | `[Red]`, `[Blue]`, `[Green]`, `[Yellow]`, `[Magenta]`, `[Cyan]`, `[White]`, `[Black]`, `[Color1]..[Color56]` | Resolved to deterministic hex color strings on `FormattedValue::color`. |
| Number placeholders | `0`, `#`, `?` | Fixed zero-padding (`0`), optional digits (`#`), and space-aligned fraction/decimal placeholders (`?`), with half-way-away-from-zero rounding. |
| Grouping & scaling | `,` | Interior `,` groups thousands (`1,234,567`); trailing `,` scales by $10^{-3}$ per comma (`1,200,000` with `0.0,,` → `1.2`). |
| Percentage | `%` | Multiplies numeric value by `100` and emits `%`. |
| Scientific notation | `0.00E+00`, `##0.0E-0` | Mantissa scaling and signed/unsigned exponent zero-padding. |
| Fractions | `# ?/?`, `# ??/??`, `# ???/???`, `# ?/8` | Continued-fraction best rational approximation up to the max denominator digit count, or fixed denominator (`2`, `4`, `8`, `16`, `10`, `100`). |
| Dates & times | `yyyy`, `yy`, `mmmm`, `mmm`, `mm`, `m`, `dddd`, `ddd`, `dd`, `d`, `hh`, `h`, `ss`, `.000`, `AM/PM`, `A/P` | Contextual `m` disambiguation (`minute` when immediately following `h`/`hh` or preceding `s`/`ss`, `month` otherwise). |
| Elapsed time | `[h]:mm:ss`, `[m]:ss`, `[s]` | Unbounded cumulative hours, minutes, or seconds for durations `>= 24h`. |
| Date systems | `DateSystem::Date1900`, `DateSystem::Date1904` | `Date1900` faithfully reproduces the Lotus 1-2-3 leap-year bug (`serial 60` = `1900-02-29`). `Date1904` starts at `1904-01-01` (`serial 0`). |
| Escapes & literals | `"text"`, `\c`, `_x`, `*x`, `@` | Quoted literals, backslash-escaped characters, `_` width spacer (emitted as space), `*` repeat fill recorded in `FormattedValue::fill_char`, and `@` text placeholder. |
| Forensic concealment | `;;;`, `;;` | Detected via `is_concealed_format(code)` and flagged on `FormattedValue::is_concealed` so concealed cell values are surfaced during forensic review. |

## Rounding and display rules

- Fixed-point values are reduced to 15 significant digits before decimal rounding, as Excel does, so `1.005` with `0.00` shows `1.01` and `1E+20` with `0` shows `100000000000000000000`.
- A format with a decimal point keeps it when every optional decimal is blank (`2` with `0.##` shows `2.`).
- Times are rounded to the displayed precision (whole seconds, or `ss.0`..`ss.000`) before they are split into fields: `0.9999999` with `hh:mm:ss` shows `00:00:00`.
- Weekdays follow Excel's serials, where serial 1 (1900-01-01) is a Sunday.
- A number in a text format (`@`, Excel's "Text" format) is shown as `General`.

## Conformance status

The Excel-generated conformance fixture (thousands of value/format pairs with Excel's `Range.Text`) is **not in place yet**, so the ≥ 99.5% target is unmeasured. The unit tests cover about 100 hand-checked pairs.

Known differences from Excel:

- Date serials outside `0..=2958465` are formatted instead of shown as `#######`.
- `*` repeat fill is recorded in `FormattedValue::fill_char` but not expanded to the cell width.
- `General` uses the 11-character rule regardless of the column width.
- Scientific and fraction formats round the binary value, not the 15-digit value.
- Only en-US names and separators are built in.
