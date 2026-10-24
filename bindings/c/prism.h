/* SPDX-License-Identifier: AGPL-3.0-only */
/**
 * @file prism.h
 * @brief C ABI header for the Prism Document Viewing & Conversion SDK.
 *
 * Conventions:
 * - Input buffers are `(data, len)` pairs; `data` may only be NULL when `len` is 0.
 * - `filename` is an optional NUL-terminated UTF-8 string (may be NULL).
 * - Returned `char*` strings are NUL-terminated UTF-8 owned by the caller and must be
 *   released with `prism_free_string()`, EXCEPT `prism_version()` and `prism_last_error()`.
 * - Returned `(uint8_t* out_buf, size_t out_len)` buffers must be released with
 *   `prism_buffer_free(out_buf, out_len)`.
 * - Opaque `PrismDocument*` handles returned by `prism_document_open()` must be released
 *   with `prism_document_close(doc)`.
 * - No function unwinds across the FFI boundary: panics are caught and reported via
 *   `prism_last_error()` on the calling thread.
 */

#ifndef PRISM_H
#define PRISM_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Opaque handle to a parsed and positioned Prism document (`LayoutDocument`). */
typedef struct PrismDocument PrismDocument;

/** Opaque handle to a parsed Excel workbook (`prism_excel::Workbook`) for grid-mode inspection. */
typedef struct PrismWorkbook PrismWorkbook;

/** Page render format: standalone UTF-8 SVG document (`<svg ...>...</svg>`). */
#define PRISM_PAGE_FORMAT_SVG             0u
/** Page render format: 8-bit RGBA PNG image bytes. */
#define PRISM_PAGE_FORMAT_PNG             1u
/** Page render format: UTF-8 HTML `<section class="prism-page">` fragment with SVG + text layer. */
#define PRISM_PAGE_FORMAT_VIEWER_FRAGMENT 2u

/** Document export format: searchable PDF 1.7 document bytes. */
#define PRISM_EXPORT_FORMAT_PDF           0u
/** Document export format: multi-page RGBA TIFF document bytes. */
#define PRISM_EXPORT_FORMAT_TIFF          1u
/** Document export format: self-contained HTML5 viewer document bytes. */
#define PRISM_EXPORT_FORMAT_HTML_VIEWER   2u

/** Search flag bit 0: case-sensitive matching. */
#define PRISM_SEARCH_CASE_SENSITIVE       (1u << 0)
/** Search flag bit 1: ignore combining diacritical marks (accents). */
#define PRISM_SEARCH_IGNORE_DIACRITICS    (1u << 1)
/** Search flag bit 2: match whole words only. */
#define PRISM_SEARCH_WHOLE_WORD           (1u << 2)

/** Tracked-changes markup mode: show insertions, deletions, change bars, and comments (`AllMarkup`). */
#define PRISM_MARKUP_ALL                  0u
/** Tracked-changes markup mode: final text with left-margin change bars (`SimpleMarkup`). */
#define PRISM_MARKUP_SIMPLE               1u
/** Tracked-changes markup mode: final text as if all revisions were accepted (`NoMarkup`). */
#define PRISM_MARKUP_NONE                 2u
/** Tracked-changes markup mode: original text as if all revisions were rejected (`Original`). */
#define PRISM_MARKUP_ORIGINAL             3u

/** Initialize logging (optional). Honors the `RUST_LOG` environment variable. */
void prism_init(void);

/** Library version string (e.g. `"0.5.0"`). Static lifetime; do NOT free. */
const char* prism_version(void);

/**
 * Error message for the last failed call on the current thread, or NULL if it succeeded.
 * Valid until the next Prism call on the same thread; do NOT free.
 */
const char* prism_last_error(void);

/** Free a NUL-terminated string returned by Prism. Passing NULL is a no-op. */
void prism_free_string(char* s);

/** Free a byte buffer returned by `prism_document_render_page` or `prism_document_export`. */
void prism_buffer_free(uint8_t* buf, size_t len);

/* ========================================================================== *
 * Whole-document convenience API                                             *
 * ========================================================================== */

char* prism_detect_format(const uint8_t* data, size_t len);
char* prism_detect_format_ex(const uint8_t* data, size_t len, const char* filename);
char* prism_convert_to_html(const uint8_t* data, size_t len);
char* prism_convert_to_html_ex(const uint8_t* data, size_t len, const char* filename);
char* prism_extract_text(const uint8_t* data, size_t len, const char* filename);
char* prism_extract_metadata(const uint8_t* data, size_t len, const char* filename);
char* prism_preview_file(const uint8_t* data, size_t len);

/* ========================================================================== *
 * Page-on-demand API (`PrismDocument`)                                       *
 * ========================================================================== */

/**
 * Open and lay out a document from an in-memory buffer.
 * Returns a non-NULL `PrismDocument*` on success, or NULL on error (`prism_last_error()`).
 */
PrismDocument* prism_document_open(const uint8_t* data, size_t len, const char* filename);

/** Close and free a `PrismDocument*`. Passing NULL is a safe no-op. */
void prism_document_close(PrismDocument* doc);

/** Return the number of laid-out pages in `doc` (or 0 if `doc` is NULL). */
uint32_t prism_document_page_count(const PrismDocument* doc);

/**
 * Query the dimensions of 0-based `page` in points (1/72 inch).
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_page_size(
    const PrismDocument* doc,
    uint32_t page,
    float* w_pt,
    float* h_pt
);

/**
 * Render a single 0-based `page` to `PRISM_PAGE_FORMAT_SVG` (0), `PRISM_PAGE_FORMAT_PNG` (1),
 * or `PRISM_PAGE_FORMAT_VIEWER_FRAGMENT` (2).
 * Release `*out_buf` with `prism_buffer_free(*out_buf, *out_len)`.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_render_page(
    const PrismDocument* doc,
    uint32_t page,
    uint32_t format,
    float dpi,
    uint8_t** out_buf,
    size_t* out_len
);

/**
 * Extract reading-order plain text for a 0-based `page`.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_page_text(const PrismDocument* doc, uint32_t page);

/**
 * Search across all pages of `doc` for `query` with `flags` (`PRISM_SEARCH_*`).
 * Returns a JSON array of `SearchHit` objects; release with `prism_free_string()`.
 */
char* prism_document_search(
    const PrismDocument* doc,
    const char* query,
    uint32_t flags
);

/**
 * Export the entire document to `PRISM_EXPORT_FORMAT_PDF` (0),
 * `PRISM_EXPORT_FORMAT_TIFF` (1), or `PRISM_EXPORT_FORMAT_HTML_VIEWER` (2).
 * Release `*out_buf` with `prism_buffer_free(*out_buf, *out_len)`.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_export(
    const PrismDocument* doc,
    uint32_t format,
    uint8_t** out_buf,
    size_t* out_len
);

/**
 * Return the per-document font resolution and substitution report as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_font_report(const PrismDocument* doc);

/**
 * Set the tracked-changes markup mode (`PRISM_MARKUP_*`) and re-layout if `doc` is a Word document.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_set_markup_mode(PrismDocument* doc, uint32_t mode);

/**
 * Return the per-document forensic facts (Word, PowerPoint, or Excel) as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_forensic_facts(const PrismDocument* doc);

/**
 * Generate the cross-format `HiddenContentReport` (`items` and `coverage`) for `doc` as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_hidden_content(const PrismDocument* doc);

/**
 * Build the unified container and embedded-object `FamilyTree` for `doc` as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_children(const PrismDocument* doc);

/**
 * Open a child container/embedded-object node (`node_id`, e.g. `"root/0"`) from `doc` as a new `PrismDocument*`.
 * Release the returned handle with `prism_document_close()`.
 */
PrismDocument* prism_document_open_child(const PrismDocument* doc, const char* node_id);

/**
 * Load and validate an `AnnotationSidecar` JSON string into `doc`.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_load_annotations(PrismDocument* doc, const char* sidecar_json);

/**
 * Serialize `doc`'s current `AnnotationSidecar` to deterministic JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_save_annotations(const PrismDocument* doc);

/**
 * Add a single `Annotation` (serialized as JSON) to `doc`'s sidecar.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_add_annotation(PrismDocument* doc, const char* annotation_json);

/**
 * Remove an annotation by `annotation_id` from `doc`'s sidecar.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_remove_annotation(PrismDocument* doc, const char* annotation_id);

/**
 * Resolve all annotations in `doc`'s sidecar against `doc`'s current layout (`Exact`, `Moved`, `Unresolved`) as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_annotations(const PrismDocument* doc);

/**
 * Export `doc` with all approved redactions destructively applied and verified automatically.
 * `format`: `PRISM_EXPORT_FORMAT_PDF` (0) or `PRISM_EXPORT_FORMAT_TIFF` (1).
 * Release `*out_buf` with `prism_buffer_free(*out_buf, *out_len)` and `*out_log_json` (if non-NULL)
 * with `prism_free_string(*out_log_json)`.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_document_export_redacted(
    const PrismDocument* doc,
    uint32_t format,
    float dpi,
    uint8_t** out_buf,
    size_t* out_len,
    char** out_log_json
);

/* ========================================================================== *
 * Workbook Grid-Mode API (`PrismWorkbook`)                                   *
 * ========================================================================== */

/**
 * Open an Excel workbook (`.xlsx`, `.xlsm`, or `.xls`) from an in-memory buffer.
 * Returns a non-NULL `PrismWorkbook*` on success, or NULL on error (`prism_last_error()`).
 */
PrismWorkbook* prism_workbook_open(const uint8_t* data, size_t len, const char* filename);

/** Close and free a `PrismWorkbook*`. Passing NULL is a safe no-op. */
void prism_workbook_close(PrismWorkbook* wb);

/**
 * Return workbook sheet descriptors (`index`, `name`, `visibility`, `tab_color`, `used_rows`,
 * `used_cols`, `populated_cells`) and workbook-level forensic facts as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_workbook_sheets(const PrismWorkbook* wb);

/**
 * Return the grid layout metadata for 0-based `sheet`: column spans, explicit row metrics,
 * merged ranges, frozen panes, tables, hyperlinks, comments, conditional formatting rules,
 * and print setup as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_workbook_sheet_layout(const PrismWorkbook* wb, uint32_t sheet);

/**
 * Query populated cells in the 0-based inclusive rectangle `[r0..=r1, c0..=c1]` on `sheet` as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_workbook_cells(
    const PrismWorkbook* wb,
    uint32_t sheet,
    uint32_t r0,
    uint32_t c0,
    uint32_t r1,
    uint32_t c1
);

/**
 * Return the deduplicated workbook `StylesTable` (`num_fmts`, `fonts`, `fills`, `borders`,
 * `cell_xfs`, `dxfs`) as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_workbook_styles(const PrismWorkbook* wb);

/**
 * Return the floating drawing objects (charts, images, shapes) anchored on 0-based `sheet` as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_workbook_drawings(const PrismWorkbook* wb, uint32_t sheet);

/**
 * Render a single floating drawing object (`object_id`, 0-based index on `sheet`) to
 * `PRISM_PAGE_FORMAT_SVG` (0) or `PRISM_PAGE_FORMAT_PNG` (1).
 * Release `*out_buf` with `prism_buffer_free(*out_buf, *out_len)`.
 * Returns 0 on success, -1 on error.
 */
int32_t prism_workbook_render_object(
    const PrismWorkbook* wb,
    uint32_t sheet,
    uint32_t object_id,
    uint32_t format,
    float dpi,
    uint8_t** out_buf,
    size_t* out_len
);

/**
 * Search across all sheets of `wb` (including `hidden` and `veryHidden` sheets, displayed text,
 * raw values, formulas, and comments) for `query` and return matching hits as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_workbook_search(
    const PrismWorkbook* wb,
    const char* query,
    uint32_t flags
);

#ifdef __cplusplus
}
#endif

#endif /* PRISM_H */
