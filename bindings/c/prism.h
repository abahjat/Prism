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
 * Return the per-document Word forensic facts (macros, external links, revisions, comments) as JSON.
 * Release the returned string with `prism_free_string()`.
 */
char* prism_document_forensic_facts(const PrismDocument* doc);

#ifdef __cplusplus
}
#endif

#endif /* PRISM_H */
