// SPDX-License-Identifier: AGPL-3.0-only
/**
 * Prism Interactive Document Viewer (`prism-viewer.ts`)
 *
 * Self-contained, offline viewer for `LayoutDocument` pages rendered as SVG + selectable
 * text layers. Supports two data sources:
 * 1. Embedded pages (`section.prism-page` elements or `#prism-viewer-data` JSON script).
 * 2. Host callback (`window.PrismHost.renderPage(pageIndex)` / `window.PrismHost.search(query, flags)`)
 *    for page-on-demand native hosts (WebView2 / .NET / C++).
 *
 * Features:
 * - Virtualized page DOM mounting via IntersectionObserver
 * - Page navigation (prev/next, direct page number input, Home/End)
 * - Zoom (`fit-width`, `fit-page`, explicit percentage, +/- buttons & shortcuts)
 * - Rotation (`0°`, `90°`, `180°`, `270°` CW/CCW)
 * - Toggleable thumbnail sidebar
 * - Client-side & host-assisted search with highlighted bounding boxes, case-sensitivity
 *   and whole-word options, next/prev navigation, and live ARIA status announcements
 * - Full keyboard navigation and ARIA accessibility labels
 */

export interface PageDescriptor {
  index: number;
  widthPt: number;
  heightPt: number;
  label?: string;
  htmlFragment?: string;
}

export interface SearchRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface SearchHit {
  page: number;
  text: string;
  rects: SearchRect[];
}

export interface WorkbookSheetDescriptor {
  index: number;
  name: string;
  visibility: "Visible" | "Hidden" | "VeryHidden";
  tab_color?: { r: number; g: number; b: number; a: number } | null;
  used_rows: number;
  used_cols: number;
  populated_cells: number;
}

export interface WorkbookCellRecord {
  row: number;
  col: number;
  a1: string;
  value: unknown;
  display_text: string;
  formula?: { expression: string } | null;
  num_fmt_code?: string | null;
  is_concealed?: boolean;
  has_comment?: boolean;
}

export interface WorkbookSheetLayout {
  frozen_rows?: number;
  frozen_cols?: number;
  default_row_height_pt?: number;
  default_col_width_pt?: number;
  merged_ranges?: Array<{ start_row: number; start_col: number; end_row: number; end_col: number }>;
}

/**
 * Virtualized high-DPI 2D `<canvas>` spreadsheet grid renderer supporting frozen panes,
 * column/row headers, comment indicators, concealed badges, cell selection, and cell-range
 * redaction selection.
 */
export class SpreadsheetCanvasGrid {
  private readonly canvas: HTMLCanvasElement;
  private readonly onSelectCell: (cell: WorkbookCellRecord | null, rangeA1: string) => void;
  private sheetIndex = 0;
  private scrollRow = 0;
  private scrollCol = 0;
  private frozenRows = 0;
  private frozenCols = 0;
  private selectedRange = { r0: 0, c0: 0, r1: 0, c1: 0 };
  private cellMap: Map<string, WorkbookCellRecord> = new Map();

  constructor(
    canvas: HTMLCanvasElement,
    onSelectCell: (cell: WorkbookCellRecord | null, rangeA1: string) => void
  ) {
    this.canvas = canvas;
    this.onSelectCell = onSelectCell;
    this.bindEvents();
  }

  public async loadSheet(sheetIndex: number): Promise<void> {
    this.sheetIndex = sheetIndex;
    this.scrollRow = 0;
    this.scrollCol = 0;
    this.cellMap.clear();
    if (window.PrismHost?.getWorkbookSheetLayout) {
      const layout = await window.PrismHost.getWorkbookSheetLayout(sheetIndex);
      this.frozenRows = layout?.frozen_rows || 0;
      this.frozenCols = layout?.frozen_cols || 0;
    } else {
      this.frozenRows = 0;
      this.frozenCols = 0;
    }
    await this.fetchVisibleTile();
    this.render();
  }

  private async fetchVisibleTile(): Promise<void> {
    if (!window.PrismHost?.getWorkbookCells) return;
    const r0 = Math.max(0, this.scrollRow);
    const c0 = Math.max(0, this.scrollCol);
    const res = await window.PrismHost.getWorkbookCells(this.sheetIndex, r0, c0, r0 + 50, c0 + 20);
    for (const c of res?.cells || []) {
      this.cellMap.set(`${c.row}:${c.col}`, c);
    }
    if (this.frozenRows > 0 || this.frozenCols > 0) {
      const fRes = await window.PrismHost.getWorkbookCells(
        this.sheetIndex,
        0,
        0,
        Math.max(0, this.frozenRows - 1),
        Math.max(0, this.frozenCols - 1)
      );
      for (const c of fRes?.cells || []) {
        this.cellMap.set(`${c.row}:${c.col}`, c);
      }
    }
  }

  public static colToLetters(col: number): string {
    let n = col + 1;
    let out = "";
    while (n > 0) {
      const rem = (n - 1) % 26;
      out = String.fromCharCode(65 + rem) + out;
      n = Math.floor((n - 1) / 26);
    }
    return out;
  }

  private bindEvents(): void {
    this.canvas.addEventListener("wheel", (e: WheelEvent) => {
      e.preventDefault();
      this.scrollRow = Math.max(0, this.scrollRow + (e.deltaY > 0 ? 3 : -3));
      this.scrollCol = Math.max(0, this.scrollCol + (e.deltaX > 0 ? 1 : e.deltaX < 0 ? -1 : 0));
      void this.fetchVisibleTile().then(() => this.render());
    });

    this.canvas.addEventListener("mousedown", (e: MouseEvent) => {
      const rect = this.canvas.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      if (x < 44 || y < 24) return;
      const col = this.scrollCol + Math.floor((x - 44) / 96);
      const row = this.scrollRow + Math.floor((y - 24) / 22);
      if (e.shiftKey) {
        this.selectedRange.r1 = row;
        this.selectedRange.c1 = col;
      } else {
        this.selectedRange = { r0: row, c0: col, r1: row, c1: col };
      }
      const cell = this.cellMap.get(`${row}:${col}`) || null;
      const a1Start = `${SpreadsheetCanvasGrid.colToLetters(this.selectedRange.c0)}${this.selectedRange.r0 + 1}`;
      const a1End = `${SpreadsheetCanvasGrid.colToLetters(this.selectedRange.c1)}${this.selectedRange.r1 + 1}`;
      const rangeA1 = a1Start === a1End ? a1Start : `${a1Start}:${a1End}`;
      this.onSelectCell(cell, rangeA1);
      this.render();
    });
  }

  public render(): void {
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    const width = this.canvas.clientWidth || 800;
    const height = this.canvas.clientHeight || 520;
    this.canvas.width = Math.floor(width * dpr);
    this.canvas.height = Math.floor(height * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const headerW = 44;
    const headerH = 24;
    const colW = 96;
    const rowH = 22;
    const colsVisible = Math.ceil((width - headerW) / colW);
    const rowsVisible = Math.ceil((height - headerH) / rowH);

    ctx.fillStyle = "#ffffff";
    ctx.fillRect(0, 0, width, height);

    // Column headers
    ctx.fillStyle = "#f1f5f9";
    ctx.fillRect(0, 0, width, headerH);
    ctx.fillRect(0, 0, headerW, height);
    ctx.strokeStyle = "#cbd5e1";
    ctx.lineWidth = 1;
    ctx.font = "600 11px sans-serif";
    ctx.fillStyle = "#334155";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";

    for (let c = 0; c < colsVisible; c++) {
      const colIdx = this.scrollCol + c;
      const x = headerW + c * colW;
      ctx.strokeRect(x, 0, colW, headerH);
      ctx.fillText(SpreadsheetCanvasGrid.colToLetters(colIdx), x + colW / 2, headerH / 2);
    }

    for (let r = 0; r < rowsVisible; r++) {
      const rowIdx = this.scrollRow + r;
      const y = headerH + r * rowH;
      ctx.strokeRect(0, y, headerW, rowH);
      ctx.fillText(String(rowIdx + 1), headerW / 2, y + rowH / 2);
    }

    // Cells
    ctx.font = "12px sans-serif";
    ctx.textAlign = "left";
    for (let r = 0; r < rowsVisible; r++) {
      const rowIdx = this.scrollRow + r;
      const y = headerH + r * rowH;
      for (let c = 0; c < colsVisible; c++) {
        const colIdx = this.scrollCol + c;
        const x = headerW + c * colW;
        ctx.strokeStyle = "#e2e8f0";
        ctx.strokeRect(x, y, colW, rowH);

        const cell = this.cellMap.get(`${rowIdx}:${colIdx}`);
        if (cell) {
          ctx.fillStyle = cell.is_concealed ? "#b45309" : "#0f172a";
          const txt = cell.display_text || "";
          ctx.fillText(txt.slice(0, 14), x + 4, y + rowH / 2);
          if (cell.has_comment) {
            ctx.fillStyle = "#dc2626";
            ctx.beginPath();
            ctx.moveTo(x + colW - 6, y);
            ctx.lineTo(x + colW, y);
            ctx.lineTo(x + colW, y + 6);
            ctx.closePath();
            ctx.fill();
          }
        }

        const rMin = Math.min(this.selectedRange.r0, this.selectedRange.r1);
        const rMax = Math.max(this.selectedRange.r0, this.selectedRange.r1);
        const cMin = Math.min(this.selectedRange.c0, this.selectedRange.c1);
        const cMax = Math.max(this.selectedRange.c0, this.selectedRange.c1);
        if (rowIdx >= rMin && rowIdx <= rMax && colIdx >= cMin && colIdx <= cMax) {
          ctx.strokeStyle = "#2563eb";
          ctx.lineWidth = 2;
          ctx.strokeRect(x + 1, y + 1, colW - 2, rowH - 2);
          ctx.lineWidth = 1;
        }
      }
    }

    // Frozen pane divider lines
    if (this.frozenRows > 0) {
      const fy = headerH + this.frozenRows * rowH;
      ctx.strokeStyle = "#475569";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(0, fy);
      ctx.lineTo(width, fy);
      ctx.stroke();
      ctx.lineWidth = 1;
    }
    if (this.frozenCols > 0) {
      const fx = headerW + this.frozenCols * colW;
      ctx.strokeStyle = "#475569";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(fx, 0);
      ctx.lineTo(fx, height);
      ctx.stroke();
      ctx.lineWidth = 1;
    }
  }
}

export interface FindingLocation {
  location_type: "page" | "slide" | "sheet" | "part_path" | "document";
  page_index?: number;
  slide_index?: number;
  sheet_name?: string;
  cell_range?: string;
  path?: string;
  rects?: SearchRect[];
}

export interface FindingItem {
  kind: string;
  severity: "info" | "notable" | "high";
  location: FindingLocation;
  excerpt?: string;
  detail?: unknown;
}

export interface ViewerAnnotation {
  id: string;
  kind: { type: string; reason_code?: string; label?: string; color?: string };
  page_index: number;
  rects_pct: SearchRect[];
  proposed?: boolean;
}

export interface PrismHostBridge {
  getPageCount?: () => Promise<number> | number;
  getPageSize?: (pageIndex: number) => Promise<{ widthPt: number; heightPt: number }> | { widthPt: number; heightPt: number };
  renderPageFragment?: (pageIndex: number) => Promise<string> | string;
  searchDocument?: (query: string, caseSensitive: boolean, wholeWord: boolean) => Promise<SearchHit[]> | SearchHit[];
  /** Re-lay out the document in a `PRISM_MARKUP_*` mode; pages are then fetched again. */
  setMarkupMode?: (mode: number) => Promise<void> | void;
  /** Workbook Grid-Mode host bridge (`prism_workbook_*`). */
  getWorkbookSheets?: () => Promise<{ sheets: WorkbookSheetDescriptor[] }> | { sheets: WorkbookSheetDescriptor[] };
  getWorkbookSheetLayout?: (sheet: number) => Promise<WorkbookSheetLayout> | WorkbookSheetLayout;
  getWorkbookCells?: (sheet: number, r0: number, c0: number, r1: number, c1: number) => Promise<{ cells: WorkbookCellRecord[] }> | { cells: WorkbookCellRecord[] };
  /** Phase 4 Review & Findings bridge (`prism_document_hidden_content` / `prism_document_annotations`). */
  getHiddenContentReport?: () => Promise<{ items: FindingItem[] }> | { items: FindingItem[] };
  getAnnotations?: () => Promise<ViewerAnnotation[]> | ViewerAnnotation[];
}

declare global {
  interface Window {
    PrismHost?: PrismHostBridge;
  }
}

type ZoomMode = "fit-width" | "fit-page" | number;

export class PrismViewerController {
  private readonly root: HTMLElement;
  private readonly viewport: HTMLElement;
  private readonly pagesContainer: HTMLElement;
  private readonly thumbsSidebar: HTMLElement | null;
  private readonly pageSlots: HTMLElement[] = [];
  private readonly pageDescriptors: PageDescriptor[] = [];
  private readonly mountedPages: Set<number> = new Set();

  private currentPageIndex = 0;
  private zoomMode: ZoomMode = "fit-width";
  private currentScale = 1.0;
  private rotationDeg = 0;
  private searchHits: SearchHit[] = [];
  private activeHitIndex = -1;
  private findings: FindingItem[] = [];
  private readonly revealedKinds: Set<string> = new Set();
  private activeFindingRects: Array<{ page: number; rect: SearchRect }> = [];
  private annotations: ViewerAnnotation[] = [];
  private previewRedactions = true;
  private readonly reviewPanel: HTMLElement | null;
  private observer: IntersectionObserver | null = null;

  constructor(root: HTMLElement) {
    this.root = root;
    this.viewport = (root.querySelector(".prism-viewport") as HTMLElement) || root;
    this.pagesContainer = (root.querySelector(".prism-viewer") as HTMLElement) || root;
    this.thumbsSidebar = root.querySelector(".prism-thumbs-sidebar");
    this.reviewPanel = root.querySelector("#prism-review-panel");
  }

  public async init(): Promise<void> {
    await this.discoverPages();
    this.setupToolbar();
    this.setupThumbnails();
    this.setupVirtualization();
    this.setupKeyboardShortcuts();
    await this.setupWorkbookGrid();
    await this.setupReviewPanel();
    this.applyZoomAndRotation();

    window.addEventListener("resize", () => {
      if (this.zoomMode === "fit-width" || this.zoomMode === "fit-page") {
        this.applyZoomAndRotation();
      }
    });
  }

  /** Ask the host to lay the document out again in `mode`, then remount the visible pages. */
  private async relayout(mode: number): Promise<void> {
    const setMarkupMode = window.PrismHost?.setMarkupMode;
    if (!setMarkupMode) return;
    await setMarkupMode(mode);
    this.root.setAttribute("data-markup-mode", String(mode));
    this.pageDescriptors.forEach((d) => {
      d.htmlFragment = undefined;
    });
    for (const idx of Array.from(this.mountedPages)) {
      this.mountedPages.delete(idx);
      await this.mountPage(idx);
    }
  }

  private async setupWorkbookGrid(): Promise<void> {
    if (!window.PrismHost?.getWorkbookSheets) {
      return;
    }
    const info = await window.PrismHost.getWorkbookSheets();
    const sheets = info?.sheets || [];
    if (sheets.length === 0) {
      return;
    }
    const formulaBar = this.root.querySelector<HTMLElement>("#prism-formula-bar");
    const sheetTabs = this.root.querySelector<HTMLElement>("#prism-sheet-tabs");
    const hiddenWrap = this.root.querySelector<HTMLElement>("#prism-forensic-hidden-wrap");
    const concealedWrap = this.root.querySelector<HTMLElement>("#prism-forensic-concealed-wrap");
    const hiddenToggle = this.root.querySelector<HTMLInputElement>("#prism-toggle-hidden-sheets");
    const concealedToggle = this.root.querySelector<HTMLInputElement>("#prism-toggle-concealed");
    if (formulaBar) formulaBar.hidden = false;
    if (sheetTabs) sheetTabs.hidden = false;
    if (hiddenWrap) hiddenWrap.hidden = false;
    // Concealed (`;;;`) values are already shown, in brackets, whenever hidden content is
    // shown, so the separate reveal toggle stays hidden.
    if (concealedWrap) concealedWrap.hidden = true;
    // Review mode: hidden and veryHidden sheets are listed by default.
    if (hiddenToggle) hiddenToggle.checked = true;

    let activeSheet = 0;
    const gridContainer = this.root.querySelector<HTMLElement>("#prism-spreadsheet-grid-container");
    const gridCanvas = this.root.querySelector<HTMLCanvasElement>("#prism-spreadsheet-canvas");
    const gridModeBtn = this.root.querySelector<HTMLButtonElement>("#prism-btn-sheet-grid-mode");
    let canvasGrid: SpreadsheetCanvasGrid | null = null;
    if (gridCanvas) {
      canvasGrid = new SpreadsheetCanvasGrid(gridCanvas, (cell, rangeA1) => {
        const refEl = this.root.querySelector<HTMLElement>("#prism-cell-ref");
        const exprEl = this.root.querySelector<HTMLElement>("#prism-formula-expr");
        const metaEl = this.root.querySelector<HTMLElement>("#prism-cell-meta");
        if (refEl) refEl.textContent = rangeA1;
        if (cell) {
          if (exprEl) {
            exprEl.textContent = cell.formula?.expression
              ? `=${cell.formula.expression}  (cached: ${cell.display_text})`
              : cell.display_text;
          }
          if (metaEl) {
            const fmt = cell.num_fmt_code || "General";
            const conc = cell.is_concealed ? " [concealed ;;;]" : "";
            metaEl.textContent = `numFmt: ${fmt}${conc}`;
          }
        }
      });
    }
    gridModeBtn?.addEventListener("click", () => {
      if (!gridContainer) return;
      const toGrid = gridContainer.hidden;
      gridContainer.hidden = !toGrid;
      this.pagesContainer.hidden = toGrid;
      gridModeBtn.textContent = toGrid ? "Print View" : "Grid View";
      if (toGrid && canvasGrid) {
        void canvasGrid.loadSheet(activeSheet);
      }
    });
    const renderTabs = (): void => {
      if (!sheetTabs) return;
      sheetTabs.innerHTML = "";
      const showHidden = Boolean(hiddenToggle?.checked);
      sheets.forEach((s) => {
        if (s.visibility !== "Visible" && !showHidden) return;
        const btn = document.createElement("button");
        btn.type = "button";
        btn.role = "tab";
        btn.className = s.index === activeSheet ? "prism-sheet-tab prism-sheet-tab-active" : "prism-sheet-tab";
        btn.textContent = s.name;
        if (s.visibility !== "Visible") {
          const badge = document.createElement("span");
          badge.className = "prism-sheet-badge";
          badge.textContent = s.visibility === "VeryHidden" ? "veryHidden" : "hidden";
          btn.appendChild(badge);
        }
        btn.addEventListener("click", () => {
          activeSheet = s.index;
          renderTabs();
          // A sheet spans several pages; go to its first one.
          const first = this.pageDescriptors.find((d) => d.label === s.name);
          if (first) this.goToPage(first.index);
          void inspectFirstCell(activeSheet);
        });
        sheetTabs.appendChild(btn);
      });
    };

    const inspectFirstCell = async (sheetIdx: number): Promise<void> => {
      if (!window.PrismHost?.getWorkbookCells) return;
      const res = await window.PrismHost.getWorkbookCells(sheetIdx, 0, 0, 25, 15);
      const first = res?.cells?.[0];
      const refEl = this.root.querySelector<HTMLElement>("#prism-cell-ref");
      const exprEl = this.root.querySelector<HTMLElement>("#prism-formula-expr");
      const metaEl = this.root.querySelector<HTMLElement>("#prism-cell-meta");
      if (first) {
        if (refEl) refEl.textContent = first.a1;
        if (exprEl) {
          exprEl.textContent = first.formula?.expression
            ? `=${first.formula.expression}  (cached: ${first.display_text})`
            : first.display_text;
        }
        if (metaEl) {
          const fmt = first.num_fmt_code || "General";
          const conc = first.is_concealed ? " [concealed ;;;]" : "";
          metaEl.textContent = `numFmt: ${fmt}${conc}`;
        }
      }
    };

    hiddenToggle?.addEventListener("change", () => {
      renderTabs();
      void this.relayout(hiddenToggle.checked ? 0 : 2);
    });
    concealedToggle?.addEventListener("change", () => {
      this.root.toggleAttribute("data-reveal-concealed", Boolean(concealedToggle.checked));
    });

    renderTabs();
    await inspectFirstCell(activeSheet);
  }

  private async discoverPages(): Promise<void> {
    const existingSections = Array.from(
      this.pagesContainer.querySelectorAll<HTMLElement>("section.prism-page")
    );

    if (existingSections.length > 0) {
      existingSections.forEach((sec, idx) => {
        const w = parseFloat(sec.getAttribute("data-width-pt") || "612") || 612;
        const h = parseFloat(sec.getAttribute("data-height-pt") || "792") || 792;
        const label = sec.getAttribute("aria-label") || `Page ${idx + 1}`;
        const fragment = sec.innerHTML;

        this.pageDescriptors.push({
          index: idx,
          widthPt: w,
          heightPt: h,
          label,
          htmlFragment: fragment,
        });

        sec.classList.add("prism-page-slot");
        sec.dataset.mounted = "true";
        this.mountedPages.add(idx);
        this.pageSlots.push(sec);
      });
      return;
    }

    if (window.PrismHost && window.PrismHost.getPageCount) {
      const count = await window.PrismHost.getPageCount();
      for (let i = 0; i < count; i++) {
        const size = window.PrismHost.getPageSize
          ? await window.PrismHost.getPageSize(i)
          : { widthPt: 612, heightPt: 792 };
        this.pageDescriptors.push({
          index: i,
          widthPt: size.widthPt,
          heightPt: size.heightPt,
          label: `Page ${i + 1}`,
        });

        const slot = document.createElement("section");
        slot.className = "prism-page prism-page-slot";
        slot.dataset.page = String(i);
        slot.dataset.widthPt = String(size.widthPt);
        slot.dataset.heightPt = String(size.heightPt);
        slot.setAttribute("role", "region");
        slot.setAttribute("aria-label", `Page ${i + 1}`);
        slot.style.width = `${size.widthPt}px`;
        slot.style.height = `${size.heightPt}px`;
        this.pagesContainer.appendChild(slot);
        this.pageSlots.push(slot);
      }
    }
  }

  private setupVirtualization(): void {
    if (typeof IntersectionObserver === "undefined") {
      return;
    }
    const virtualizeThreshold = 20;
    this.observer = new IntersectionObserver(
      (entries) => {
        let bestVisiblePage = this.currentPageIndex;
        let bestRatio = 0;

        for (const entry of entries) {
          const el = entry.target as HTMLElement;
          const idx = parseInt(el.dataset.page || "0", 10);
          if (entry.isIntersecting) {
            void this.mountPage(idx);
            if (entry.intersectionRatio > bestRatio) {
              bestRatio = entry.intersectionRatio;
              bestVisiblePage = idx;
            }
          } else if (this.pageDescriptors.length > virtualizeThreshold) {
            this.unmountPage(idx);
          }
        }

        if (bestRatio > 0 && bestVisiblePage !== this.currentPageIndex) {
          this.currentPageIndex = bestVisiblePage;
          this.updatePageIndicator();
        }
      },
      {
        root: this.viewport === this.root ? null : this.viewport,
        rootMargin: "600px 0px",
        threshold: [0.01, 0.25, 0.5, 0.75],
      }
    );

    for (const slot of this.pageSlots) {
      this.observer.observe(slot);
    }
  }

  private async mountPage(pageIndex: number): Promise<void> {
    if (this.mountedPages.has(pageIndex)) {
      return;
    }
    const desc = this.pageDescriptors[pageIndex];
    const slot = this.pageSlots[pageIndex];
    if (!desc || !slot) {
      return;
    }

    if (!desc.htmlFragment && window.PrismHost?.renderPageFragment) {
      desc.htmlFragment = await window.PrismHost.renderPageFragment(pageIndex);
    }

    if (desc.htmlFragment) {
      slot.innerHTML = desc.htmlFragment;
      slot.dataset.mounted = "true";
      this.mountedPages.add(pageIndex);
      this.renderPageSearchHits(pageIndex);
    }
  }

  private unmountPage(pageIndex: number): void {
    if (!this.mountedPages.has(pageIndex)) {
      return;
    }
    const desc = this.pageDescriptors[pageIndex];
    const slot = this.pageSlots[pageIndex];
    if (!desc || !slot) {
      return;
    }
    if (!desc.htmlFragment) {
      desc.htmlFragment = slot.innerHTML;
    }
    slot.innerHTML = "";
    slot.dataset.mounted = "false";
    this.mountedPages.delete(pageIndex);
  }

  private setupToolbar(): void {
    const totalEl = this.root.querySelector<HTMLElement>("#prism-page-total");
    if (totalEl) {
      totalEl.textContent = String(this.pageDescriptors.length);
    }

    const pageInput = this.root.querySelector<HTMLInputElement>("#prism-page-input");
    if (pageInput) {
      pageInput.max = String(Math.max(1, this.pageDescriptors.length));
      pageInput.addEventListener("change", () => {
        const val = parseInt(pageInput.value, 10);
        if (!Number.isNaN(val)) {
          this.goToPage(val - 1);
        }
      });
    }

    this.root.querySelector("#prism-btn-prev")?.addEventListener("click", () => {
      this.goToPage(this.currentPageIndex - 1);
    });
    this.root.querySelector("#prism-btn-next")?.addEventListener("click", () => {
      this.goToPage(this.currentPageIndex + 1);
    });

    const zoomSelect = this.root.querySelector<HTMLSelectElement>("#prism-zoom-select");
    if (zoomSelect) {
      zoomSelect.addEventListener("change", () => {
        const val = zoomSelect.value;
        if (val === "fit-width" || val === "fit-page") {
          this.zoomMode = val;
        } else {
          this.zoomMode = parseFloat(val) || 1.0;
        }
        this.applyZoomAndRotation();
      });
    }

    this.root.querySelector("#prism-btn-zoom-in")?.addEventListener("click", () => {
      this.stepZoom(1.2);
    });
    this.root.querySelector("#prism-btn-zoom-out")?.addEventListener("click", () => {
      this.stepZoom(1 / 1.2);
    });

    this.root.querySelector("#prism-btn-rotate-cw")?.addEventListener("click", () => {
      this.rotationDeg = (this.rotationDeg + 90) % 360;
      this.applyZoomAndRotation();
    });
    this.root.querySelector("#prism-btn-rotate-ccw")?.addEventListener("click", () => {
      this.rotationDeg = (this.rotationDeg + 270) % 360;
      this.applyZoomAndRotation();
    });

    // Changing the mode needs a native host that re-lays out the document; a static export
    // keeps the mode it was rendered with, so the selector stays hidden there.
    const markupSelect = this.root.querySelector<HTMLSelectElement>("#prism-markup-select");
    const setMarkupMode = window.PrismHost?.setMarkupMode;
    if (markupSelect && setMarkupMode) {
      markupSelect.hidden = false;
      markupSelect.addEventListener("change", async () => {
        await this.relayout(parseInt(markupSelect.value, 10) || 0);
      });
    }

    this.root.querySelector("#prism-btn-thumbs")?.addEventListener("click", () => {
      if (this.thumbsSidebar) {
        const hidden = this.thumbsSidebar.hasAttribute("hidden");
        if (hidden) {
          this.thumbsSidebar.removeAttribute("hidden");
        } else {
          this.thumbsSidebar.setAttribute("hidden", "");
        }
        this.applyZoomAndRotation();
      }
    });

    const searchInput = this.root.querySelector<HTMLInputElement>("#prism-search-input");
    const caseCheckbox = this.root.querySelector<HTMLInputElement>("#prism-search-case");
    const wordCheckbox = this.root.querySelector<HTMLInputElement>("#prism-search-word");

    const triggerSearch = (): void => {
      const q = searchInput?.value || "";
      const cs = Boolean(caseCheckbox?.checked);
      const ww = Boolean(wordCheckbox?.checked);
      void this.performSearch(q, cs, ww);
    };

    searchInput?.addEventListener("input", triggerSearch);
    caseCheckbox?.addEventListener("change", triggerSearch);
    wordCheckbox?.addEventListener("change", triggerSearch);

    this.root.querySelector("#prism-search-next")?.addEventListener("click", () => {
      this.navigateSearchHit(1);
    });
    this.root.querySelector("#prism-search-prev")?.addEventListener("click", () => {
      this.navigateSearchHit(-1);
    });
  }

  private setupThumbnails(): void {
    if (!this.thumbsSidebar) {
      return;
    }
    this.thumbsSidebar.innerHTML = "";
    this.pageDescriptors.forEach((desc) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "prism-thumb-item";
      btn.dataset.page = String(desc.index);
      btn.setAttribute("aria-label", `Jump to page ${desc.index + 1}`);
      btn.innerHTML = `<span class="prism-thumb-box" style="aspect-ratio: ${desc.widthPt} / ${desc.heightPt}"></span><span class="prism-thumb-label">${desc.index + 1}</span>`;
      btn.addEventListener("click", () => this.goToPage(desc.index));
      this.thumbsSidebar?.appendChild(btn);
    });
    this.updatePageIndicator();
  }

  public goToPage(pageIndex: number): void {
    if (this.pageDescriptors.length === 0) {
      return;
    }
    const clamped = Math.max(0, Math.min(this.pageDescriptors.length - 1, pageIndex));
    this.currentPageIndex = clamped;
    this.updatePageIndicator();
    const slot = this.pageSlots[clamped];
    if (slot) {
      void this.mountPage(clamped);
      slot.scrollIntoView({ behavior: "instant", block: "start" });
    }
  }

  private updatePageIndicator(): void {
    const pageInput = this.root.querySelector<HTMLInputElement>("#prism-page-input");
    if (pageInput) {
      pageInput.value = String(this.currentPageIndex + 1);
    }
    if (this.thumbsSidebar) {
      const buttons = this.thumbsSidebar.querySelectorAll<HTMLElement>(".prism-thumb-item");
      buttons.forEach((btn, idx) => {
        if (idx === this.currentPageIndex) {
          btn.classList.add("prism-thumb-active");
          btn.setAttribute("aria-current", "page");
        } else {
          btn.classList.remove("prism-thumb-active");
          btn.removeAttribute("aria-current");
        }
      });
    }
  }

  private stepZoom(factor: number): void {
    const next = Math.max(0.25, Math.min(4.0, this.currentScale * factor));
    this.zoomMode = Math.round(next * 100) / 100;
    const zoomSelect = this.root.querySelector<HTMLSelectElement>("#prism-zoom-select");
    if (zoomSelect) {
      zoomSelect.value = "custom";
    }
    this.applyZoomAndRotation();
  }

  private applyZoomAndRotation(): void {
    if (this.pageDescriptors.length === 0) {
      return;
    }
    const first = this.pageDescriptors[this.currentPageIndex] || this.pageDescriptors[0];
    const isRotated90 = this.rotationDeg === 90 || this.rotationDeg === 270;
    const baseW = isRotated90 ? first.heightPt : first.widthPt;
    const baseH = isRotated90 ? first.widthPt : first.heightPt;

    const availW = Math.max(240, (this.viewport.clientWidth || 900) - 48);
    const availH = Math.max(240, (this.viewport.clientHeight || 760) - 48);

    if (this.zoomMode === "fit-width") {
      this.currentScale = Math.min(2.5, availW / Math.max(1, baseW));
    } else if (this.zoomMode === "fit-page") {
      this.currentScale = Math.min(2.5, Math.min(availW / Math.max(1, baseW), availH / Math.max(1, baseH)));
    } else {
      this.currentScale = this.zoomMode;
    }

    this.pageSlots.forEach((slot, idx) => {
      const desc = this.pageDescriptors[idx];
      if (!desc) return;
      const scaledW = Math.round(desc.widthPt * this.currentScale * 100) / 100;
      const scaledH = Math.round(desc.heightPt * this.currentScale * 100) / 100;
      slot.style.width = `${scaledW}px`;
      slot.style.height = `${scaledH}px`;
      slot.style.transform = this.rotationDeg === 0 ? "" : `rotate(${this.rotationDeg}deg)`;
    });
  }

  public async performSearch(
    query: string,
    caseSensitive: boolean,
    wholeWord: boolean
  ): Promise<void> {
    this.searchHits = [];
    this.activeHitIndex = -1;
    this.clearAllSearchOverlays();

    const trimmed = query.trim();
    const statusEl = this.root.querySelector<HTMLElement>("#prism-search-status");
    if (!trimmed) {
      if (statusEl) statusEl.textContent = "";
      return;
    }

    if (window.PrismHost?.searchDocument) {
      this.searchHits = await window.PrismHost.searchDocument(trimmed, caseSensitive, wholeWord);
    } else {
      this.searchHits = this.clientSideSearch(trimmed, caseSensitive, wholeWord);
    }

    if (this.searchHits.length > 0) {
      this.activeHitIndex = 0;
      this.renderAllMountedSearchHits();
      this.scrollToActiveHit();
    } else if (statusEl) {
      statusEl.textContent = "0 of 0";
    }
  }

  private clientSideSearch(
    query: string,
    caseSensitive: boolean,
    wholeWord: boolean
  ): SearchHit[] {
    const hits: SearchHit[] = [];
    const needle = caseSensitive ? query : query.toLowerCase();

    this.pageSlots.forEach((slot, pageIdx) => {
      const spans = Array.from(slot.querySelectorAll<HTMLElement>(".prism-text-span"));
      for (const span of spans) {
        const raw = span.textContent || "";
        const hay = caseSensitive ? raw : raw.toLowerCase();
        let pos = 0;
        while (pos <= hay.length - needle.length) {
          const found = hay.indexOf(needle, pos);
          if (found === -1) break;

          const beforeOk =
            !wholeWord || found === 0 || !/\w/.test(hay[found - 1] || "");
          const afterIdx = found + needle.length;
          const afterOk =
            !wholeWord || afterIdx >= hay.length || !/\w/.test(hay[afterIdx] || "");

          if (beforeOk && afterOk) {
            const desc = this.pageDescriptors[pageIdx];
            const pageW = desc ? desc.widthPt : 612;
            const pageH = desc ? desc.heightPt : 792;
            const toPctX = (v: string): number =>
              v.endsWith("%") ? parseFloat(v) || 0 : ((parseFloat(v) || 0) / pageW) * 100;
            const toPctY = (v: string): number =>
              v.endsWith("%") ? parseFloat(v) || 0 : ((parseFloat(v) || 0) / pageH) * 100;

            const leftPct = toPctX(span.style.left || "0");
            const topPct = toPctY(span.style.top || "0");
            const widthPct = toPctX(span.style.width || "30pt");
            const heightPct = toPctY(span.style.height || "12pt");
            const charLen = Math.max(1, raw.length);
            const subLeft = leftPct + (widthPct * found) / charLen;
            const subWidth = Math.max(0.5, (widthPct * needle.length) / charLen);

            hits.push({
              page: pageIdx,
              text: raw.slice(found, found + needle.length),
              rects: [{ x: subLeft, y: topPct, width: subWidth, height: heightPct }],
            });
          }
          pos = found + Math.max(1, needle.length);
        }
      }
    });

    return hits;
  }

  private navigateSearchHit(delta: number): void {
    if (this.searchHits.length === 0) {
      return;
    }
    this.activeHitIndex =
      (this.activeHitIndex + delta + this.searchHits.length) % this.searchHits.length;
    this.renderAllMountedSearchHits();
    this.scrollToActiveHit();
  }

  private scrollToActiveHit(): void {
    const statusEl = this.root.querySelector<HTMLElement>("#prism-search-status");
    if (statusEl) {
      statusEl.textContent = `${this.activeHitIndex + 1} of ${this.searchHits.length}`;
    }
    const hit = this.searchHits[this.activeHitIndex];
    if (!hit) return;
    this.goToPage(hit.page);
  }

  private clearAllSearchOverlays(): void {
    this.root.querySelectorAll(".prism-search-layer").forEach((el) => el.remove());
  }

  private renderAllMountedSearchHits(): void {
    this.clearAllSearchOverlays();
    for (const pageIdx of this.mountedPages) {
      this.renderPageSearchHits(pageIdx);
    }
  }

  private renderPageSearchHits(pageIndex: number): void {
    const slot = this.pageSlots[pageIndex];
    if (!slot) return;
    slot.querySelector(".prism-search-layer")?.remove();

    const pageHits = this.searchHits
      .map((hit, globalIdx) => ({ hit, globalIdx }))
      .filter((item) => item.hit.page === pageIndex);
    if (pageHits.length === 0) return;

    const layer = document.createElement("div");
    layer.className = "prism-search-layer";
    layer.setAttribute("aria-hidden", "true");

    for (const { hit, globalIdx } of pageHits) {
      for (const r of hit.rects) {
        const box = document.createElement("div");
        box.className =
          globalIdx === this.activeHitIndex
            ? "prism-search-hit prism-search-hit-active"
            : "prism-search-hit";
        box.style.left = `${r.x}%`;
        box.style.top = `${r.y}%`;
        box.style.width = `${r.width}%`;
        box.style.height = `${r.height}%`;
        layer.appendChild(box);
      }
    }

    slot.appendChild(layer);
  }

  private async setupReviewPanel(): Promise<void> {
    if (!this.reviewPanel) return;
    if (window.PrismHost?.getHiddenContentReport) {
      const rep = await window.PrismHost.getHiddenContentReport();
      if (rep && Array.isArray(rep.items)) {
        this.findings = rep.items;
      }
    }
    if (window.PrismHost?.getAnnotations) {
      const anns = await window.PrismHost.getAnnotations();
      if (Array.isArray(anns)) {
        this.annotations = anns;
      }
    }
  }

  private setupKeyboardShortcuts(): void {
    window.addEventListener("keydown", (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const inInput =
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "SELECT" ||
          target.tagName === "TEXTAREA");

      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        const searchInput = this.root.querySelector<HTMLInputElement>("#prism-search-input");
        if (searchInput) {
          e.preventDefault();
          searchInput.focus();
          searchInput.select();
        }
        return;
      }

      if (inInput) {
        if (target?.id === "prism-search-input" && e.key === "Enter") {
          e.preventDefault();
          this.navigateSearchHit(e.shiftKey ? -1 : 1);
        }
        return;
      }

      switch (e.key) {
        case "/": {
          const searchInput = this.root.querySelector<HTMLInputElement>("#prism-search-input");
          if (searchInput) {
            e.preventDefault();
            searchInput.focus();
          }
          break;
        }
        case "PageDown":
        case "j":
          e.preventDefault();
          this.goToPage(this.currentPageIndex + 1);
          break;
        case "PageUp":
        case "k":
          e.preventDefault();
          this.goToPage(this.currentPageIndex - 1);
          break;
        case "Home":
          e.preventDefault();
          this.goToPage(0);
          break;
        case "End":
          e.preventDefault();
          this.goToPage(this.pageDescriptors.length - 1);
          break;
        case "+":
        case "=":
          e.preventDefault();
          this.stepZoom(1.2);
          break;
        case "-":
          e.preventDefault();
          this.stepZoom(1 / 1.2);
          break;
        case "0":
          e.preventDefault();
          this.zoomMode = "fit-width";
          this.applyZoomAndRotation();
          break;
        case "r":
          e.preventDefault();
          this.rotationDeg = (this.rotationDeg + 90) % 360;
          this.applyZoomAndRotation();
          break;
        case "R":
          e.preventDefault();
          this.rotationDeg = (this.rotationDeg + 270) % 360;
          this.applyZoomAndRotation();
          break;
      }
    });
  }
}

export function bootstrapPrismViewer(): void {
  const roots = document.querySelectorAll<HTMLElement>(".prism-app-shell");
  if (roots.length > 0) {
    roots.forEach((root) => {
      const ctrl = new PrismViewerController(root);
      void ctrl.init();
    });
  } else {
    const fallback = document.querySelector<HTMLElement>(".prism-viewer");
    if (fallback && fallback.parentElement) {
      const ctrl = new PrismViewerController(fallback.parentElement);
      void ctrl.init();
    }
  }
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", bootstrapPrismViewer);
  } else {
    bootstrapPrismViewer();
  }
}
