// SPDX-License-Identifier: AGPL-3.0-only
// Bundled from viewer/src/prism-viewer.ts for self-contained offline viewing.
(function () {
  "use strict";

  class PrismViewerController {
    constructor(root) {
      this.root = root;
      this.viewport = root.querySelector(".prism-viewport") || root;
      this.pagesContainer = root.querySelector(".prism-viewer") || root;
      this.thumbsSidebar = root.querySelector(".prism-thumbs-sidebar");
      this.pageSlots = [];
      this.pageDescriptors = [];
      this.mountedPages = new Set();
      this.currentPageIndex = 0;
      this.zoomMode = "fit-width";
      this.currentScale = 1.0;
      this.rotationDeg = 0;
      this.searchHits = [];
      this.activeHitIndex = -1;
      this.findings = [];
      this.revealedKinds = new Set();
      this.activeFindingRects = [];
      this.annotations = [];
      this.previewRedactions = true;
      this.reviewPanel = root.querySelector("#prism-review-panel");
      this.observer = null;
    }

    async init() {
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

    // Ask the host to lay the document out again in `mode`, then remount the visible pages.
    async relayout(mode) {
      if (!window.PrismHost || !window.PrismHost.setMarkupMode) return;
      await window.PrismHost.setMarkupMode(mode);
      this.root.setAttribute("data-markup-mode", String(mode));
      this.pageDescriptors.forEach((d) => { d.htmlFragment = undefined; });
      for (const idx of Array.from(this.mountedPages)) {
        this.mountedPages.delete(idx);
        await this.mountPage(idx);
      }
    }

    async setupWorkbookGrid() {
      if (!window.PrismHost || !window.PrismHost.getWorkbookSheets) {
        return;
      }
      const info = await window.PrismHost.getWorkbookSheets();
      const sheets = (info && info.sheets) || [];
      if (sheets.length === 0) {
        return;
      }
      const formulaBar = this.root.querySelector("#prism-formula-bar");
      const sheetTabs = this.root.querySelector("#prism-sheet-tabs");
      const hiddenWrap = this.root.querySelector("#prism-forensic-hidden-wrap");
      const concealedWrap = this.root.querySelector("#prism-forensic-concealed-wrap");
      const hiddenToggle = this.root.querySelector("#prism-toggle-hidden-sheets");
      const concealedToggle = this.root.querySelector("#prism-toggle-concealed");
      if (formulaBar) formulaBar.hidden = false;
      if (sheetTabs) sheetTabs.hidden = false;
      if (hiddenWrap) hiddenWrap.hidden = false;
      // Concealed (`;;;`) values are already shown, in brackets, whenever hidden content is
      // shown, so the separate reveal toggle stays hidden.
      if (concealedWrap) concealedWrap.hidden = true;
      // Review mode: hidden and veryHidden sheets are listed by default.
      if (hiddenToggle) hiddenToggle.checked = true;

      let activeSheet = 0;
      const inspectFirstCell = async (sheetIdx) => {
        if (!window.PrismHost || !window.PrismHost.getWorkbookCells) return;
        const res = await window.PrismHost.getWorkbookCells(sheetIdx, 0, 0, 25, 15);
        const first = res && res.cells && res.cells[0];
        const refEl = this.root.querySelector("#prism-cell-ref");
        const exprEl = this.root.querySelector("#prism-formula-expr");
        const metaEl = this.root.querySelector("#prism-cell-meta");
        if (first) {
          if (refEl) refEl.textContent = first.a1;
          if (exprEl) {
            exprEl.textContent =
              first.formula && first.formula.expression
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

      const renderTabs = () => {
        if (!sheetTabs) return;
        sheetTabs.innerHTML = "";
        const showHidden = Boolean(hiddenToggle && hiddenToggle.checked);
        sheets.forEach((s) => {
          if (s.visibility !== "Visible" && !showHidden) return;
          const btn = document.createElement("button");
          btn.type = "button";
          btn.setAttribute("role", "tab");
          btn.className =
            s.index === activeSheet
              ? "prism-sheet-tab prism-sheet-tab-active"
              : "prism-sheet-tab";
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

      if (hiddenToggle) {
        hiddenToggle.addEventListener("change", () => {
          renderTabs();
          void this.relayout(hiddenToggle.checked ? 0 : 2);
        });
      }
      if (concealedToggle) {
        concealedToggle.addEventListener("change", () => {
          if (concealedToggle.checked) {
            this.root.setAttribute("data-reveal-concealed", "true");
          } else {
            this.root.removeAttribute("data-reveal-concealed");
          }
        });
      }

      renderTabs();
      await inspectFirstCell(activeSheet);
    }

    async discoverPages() {
      const existingSections = Array.from(
        this.pagesContainer.querySelectorAll("section.prism-page")
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

    setupVirtualization() {
      if (typeof IntersectionObserver === "undefined") {
        return;
      }
      const virtualizeThreshold = 20;
      this.observer = new IntersectionObserver(
        (entries) => {
          let bestVisiblePage = this.currentPageIndex;
          let bestRatio = 0;

          for (const entry of entries) {
            const el = entry.target;
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

    async mountPage(pageIndex) {
      if (this.mountedPages.has(pageIndex)) {
        return;
      }
      const desc = this.pageDescriptors[pageIndex];
      const slot = this.pageSlots[pageIndex];
      if (!desc || !slot) {
        return;
      }

      if (!desc.htmlFragment && window.PrismHost && window.PrismHost.renderPageFragment) {
        desc.htmlFragment = await window.PrismHost.renderPageFragment(pageIndex);
      }

      if (desc.htmlFragment) {
        slot.innerHTML = desc.htmlFragment;
        slot.dataset.mounted = "true";
        this.mountedPages.add(pageIndex);
        this.renderPageSearchHits(pageIndex);
        this.renderPageFindingOverlays(pageIndex);
        this.renderPageAnnotations(pageIndex);
      }
    }

    unmountPage(pageIndex) {
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

    setupToolbar() {
      const totalEl = this.root.querySelector("#prism-page-total");
      if (totalEl) {
        totalEl.textContent = String(this.pageDescriptors.length);
      }

      const pageInput = this.root.querySelector("#prism-page-input");
      if (pageInput) {
        pageInput.max = String(Math.max(1, this.pageDescriptors.length));
        pageInput.addEventListener("change", () => {
          const val = parseInt(pageInput.value, 10);
          if (!Number.isNaN(val)) {
            this.goToPage(val - 1);
          }
        });
      }

      const prevBtn = this.root.querySelector("#prism-btn-prev");
      if (prevBtn) prevBtn.addEventListener("click", () => this.goToPage(this.currentPageIndex - 1));
      const nextBtn = this.root.querySelector("#prism-btn-next");
      if (nextBtn) nextBtn.addEventListener("click", () => this.goToPage(this.currentPageIndex + 1));

      const zoomSelect = this.root.querySelector("#prism-zoom-select");
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

      const zoomIn = this.root.querySelector("#prism-btn-zoom-in");
      if (zoomIn) zoomIn.addEventListener("click", () => this.stepZoom(1.2));
      const zoomOut = this.root.querySelector("#prism-btn-zoom-out");
      if (zoomOut) zoomOut.addEventListener("click", () => this.stepZoom(1 / 1.2));

      const rotCw = this.root.querySelector("#prism-btn-rotate-cw");
      if (rotCw) {
        rotCw.addEventListener("click", () => {
          this.rotationDeg = (this.rotationDeg + 90) % 360;
          this.applyZoomAndRotation();
        });
      }
      const rotCcw = this.root.querySelector("#prism-btn-rotate-ccw");
      const markupSelect = this.root.querySelector("#prism-markup-select");
      // Changing the mode needs a native host that re-lays out the document; a static export
      // keeps the mode it was rendered with, so the selector stays hidden there.
      if (markupSelect && window.PrismHost && window.PrismHost.setMarkupMode) {
        markupSelect.hidden = false;
        markupSelect.addEventListener("change", async () => {
          await this.relayout(parseInt(markupSelect.value, 10) || 0);
        });
      }
      if (rotCcw) {
        rotCcw.addEventListener("click", () => {
          this.rotationDeg = (this.rotationDeg + 270) % 360;
          this.applyZoomAndRotation();
        });
      }

      const thumbsBtn = this.root.querySelector("#prism-btn-thumbs");
      if (thumbsBtn) {
        thumbsBtn.addEventListener("click", () => {
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
      }

      const searchInput = this.root.querySelector("#prism-search-input");
      const caseCheckbox = this.root.querySelector("#prism-search-case");
      const wordCheckbox = this.root.querySelector("#prism-search-word");

      const triggerSearch = () => {
        const q = searchInput ? searchInput.value : "";
        const cs = Boolean(caseCheckbox && caseCheckbox.checked);
        const ww = Boolean(wordCheckbox && wordCheckbox.checked);
        void this.performSearch(q, cs, ww);
      };

      if (searchInput) searchInput.addEventListener("input", triggerSearch);
      if (caseCheckbox) caseCheckbox.addEventListener("change", triggerSearch);
      if (wordCheckbox) wordCheckbox.addEventListener("change", triggerSearch);

      const searchNext = this.root.querySelector("#prism-search-next");
      if (searchNext) searchNext.addEventListener("click", () => this.navigateSearchHit(1));
      const searchPrev = this.root.querySelector("#prism-search-prev");
      if (searchPrev) searchPrev.addEventListener("click", () => this.navigateSearchHit(-1));

      const reviewBtn = this.root.querySelector("#prism-btn-review");
      if (reviewBtn) {
        reviewBtn.addEventListener("click", () => {
          if (this.reviewPanel) {
            if (this.reviewPanel.hasAttribute("hidden")) {
              this.reviewPanel.removeAttribute("hidden");
            } else {
              this.reviewPanel.setAttribute("hidden", "");
            }
            this.applyZoomAndRotation();
          }
        });
      }

      const proposeBtn = this.root.querySelector("#prism-btn-propose-redact");
      if (proposeBtn) {
        proposeBtn.addEventListener("click", () => this.proposeRedactionsFromSearch());
      }

      const previewToggle = this.root.querySelector("#prism-toggle-preview-redact");
      if (previewToggle) {
        previewToggle.addEventListener("change", () => {
          this.previewRedactions = Boolean(previewToggle.checked);
          this.renderAllMountedAnnotations();
        });
      }
    }

    setupThumbnails() {
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
        this.thumbsSidebar.appendChild(btn);
      });
      this.updatePageIndicator();
    }

    goToPage(pageIndex) {
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

    updatePageIndicator() {
      const pageInput = this.root.querySelector("#prism-page-input");
      if (pageInput) {
        pageInput.value = String(this.currentPageIndex + 1);
      }
      if (this.thumbsSidebar) {
        const buttons = this.thumbsSidebar.querySelectorAll(".prism-thumb-item");
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

    stepZoom(factor) {
      const next = Math.max(0.25, Math.min(4.0, this.currentScale * factor));
      this.zoomMode = Math.round(next * 100) / 100;
      const zoomSelect = this.root.querySelector("#prism-zoom-select");
      if (zoomSelect) {
        zoomSelect.value = "custom";
      }
      this.applyZoomAndRotation();
    }

    applyZoomAndRotation() {
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
        this.currentScale = Math.min(
          2.5,
          Math.min(availW / Math.max(1, baseW), availH / Math.max(1, baseH))
        );
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

    async performSearch(query, caseSensitive, wholeWord) {
      this.searchHits = [];
      this.activeHitIndex = -1;
      this.clearAllSearchOverlays();

      const trimmed = query.trim();
      const statusEl = this.root.querySelector("#prism-search-status");
      const proposeBtn = this.root.querySelector("#prism-btn-propose-redact");
      if (!trimmed) {
        if (statusEl) statusEl.textContent = "";
        if (proposeBtn) proposeBtn.hidden = true;
        return;
      }

      if (window.PrismHost && window.PrismHost.searchDocument) {
        this.searchHits = await window.PrismHost.searchDocument(trimmed, caseSensitive, wholeWord);
      } else {
        this.searchHits = this.clientSideSearch(trimmed, caseSensitive, wholeWord);
      }

      if (this.searchHits.length > 0) {
        this.activeHitIndex = 0;
        if (proposeBtn) proposeBtn.hidden = false;
        this.renderAllMountedSearchHits();
        this.scrollToActiveHit();
      } else {
        if (proposeBtn) proposeBtn.hidden = true;
        if (statusEl) statusEl.textContent = "0 of 0";
      }
    }

    clientSideSearch(query, caseSensitive, wholeWord) {
      const hits = [];
      const needle = caseSensitive ? query : query.toLowerCase();

      this.pageSlots.forEach((slot, pageIdx) => {
        const spans = Array.from(slot.querySelectorAll(".prism-text-span"));
        for (const span of spans) {
          const raw = span.textContent || "";
          const hay = caseSensitive ? raw : raw.toLowerCase();
          let pos = 0;
          while (pos <= hay.length - needle.length) {
            const found = hay.indexOf(needle, pos);
            if (found === -1) break;

            const beforeOk = !wholeWord || found === 0 || !/\w/.test(hay[found - 1] || "");
            const afterIdx = found + needle.length;
            const afterOk =
              !wholeWord || afterIdx >= hay.length || !/\w/.test(hay[afterIdx] || "");

            if (beforeOk && afterOk) {
              const desc = this.pageDescriptors[pageIdx];
              const pageW = desc ? desc.widthPt : 612;
              const pageH = desc ? desc.heightPt : 792;
              const toPctX = (v) =>
                v.endsWith("%") ? parseFloat(v) || 0 : ((parseFloat(v) || 0) / pageW) * 100;
              const toPctY = (v) =>
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

    navigateSearchHit(delta) {
      if (this.searchHits.length === 0) {
        return;
      }
      this.activeHitIndex =
        (this.activeHitIndex + delta + this.searchHits.length) % this.searchHits.length;
      this.renderAllMountedSearchHits();
      this.scrollToActiveHit();
    }

    scrollToActiveHit() {
      const statusEl = this.root.querySelector("#prism-search-status");
      if (statusEl) {
        statusEl.textContent = `${this.activeHitIndex + 1} of ${this.searchHits.length}`;
      }
      const hit = this.searchHits[this.activeHitIndex];
      if (!hit) return;
      this.goToPage(hit.page);
    }

    clearAllSearchOverlays() {
      this.root.querySelectorAll(".prism-search-layer").forEach((el) => el.remove());
    }

    renderAllMountedSearchHits() {
      this.clearAllSearchOverlays();
      for (const pageIdx of this.mountedPages) {
        this.renderPageSearchHits(pageIdx);
      }
    }

    renderPageSearchHits(pageIndex) {
      const slot = this.pageSlots[pageIndex];
      if (!slot) return;
      const old = slot.querySelector(".prism-search-layer");
      if (old) old.remove();

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

    async setupReviewPanel() {
      if (!this.reviewPanel) return;
      if (window.PrismHost && window.PrismHost.getHiddenContentReport) {
        const rep = await window.PrismHost.getHiddenContentReport();
        if (rep && Array.isArray(rep.items)) {
          this.findings = rep.items;
        }
      } else if (this.root.dataset.hiddenReport) {
        try {
          const rep = JSON.parse(this.root.dataset.hiddenReport);
          if (rep && Array.isArray(rep.items)) {
            this.findings = rep.items;
          }
        } catch (_e) {}
      }
      if (window.PrismHost && window.PrismHost.getAnnotations) {
        const anns = await window.PrismHost.getAnnotations();
        if (Array.isArray(anns)) {
          this.annotations = anns;
        }
      }
      this.renderReviewPanelContent();
    }

    renderReviewPanelContent() {
      if (!this.reviewPanel) return;
      this.reviewPanel.innerHTML = "";

      const title = document.createElement("div");
      title.style.fontWeight = "700";
      title.textContent = `Findings (${this.findings.length}) & Annotations (${this.annotations.length})`;
      this.reviewPanel.appendChild(title);

      const groups = new Map();
      for (const item of this.findings) {
        const k = item.kind || "other";
        if (!groups.has(k)) groups.set(k, []);
        groups.get(k).push(item);
      }

      groups.forEach((items, kind) => {
        const groupBox = document.createElement("div");
        groupBox.className = "prism-review-group";

        const header = document.createElement("div");
        header.className = "prism-review-group-header";
        const kindLabel = document.createElement("span");
        kindLabel.textContent = `${kind} (${items.length})`;

        const toggleLabel = document.createElement("label");
        toggleLabel.style.fontSize = "11px";
        const toggle = document.createElement("input");
        toggle.type = "checkbox";
        toggle.checked = this.revealedKinds.has(kind);
        toggle.setAttribute("data-reveal-kind", kind);
        toggle.addEventListener("change", () => {
          if (toggle.checked) {
            this.revealedKinds.add(kind);
          } else {
            this.revealedKinds.delete(kind);
          }
          this.renderAllMountedFindings();
        });
        toggleLabel.appendChild(toggle);
        toggleLabel.appendChild(document.createTextNode(" Reveal"));

        header.appendChild(kindLabel);
        header.appendChild(toggleLabel);
        groupBox.appendChild(header);

        items.forEach((finding) => {
          const btn = document.createElement("button");
          btn.type = "button";
          btn.className = "prism-finding-item";
          const sev = String(finding.severity || "info").toLowerCase();
          const locText = this.describeFindingLocation(finding.location);
          // Excerpts, sheet names and part paths come from the document: text nodes only.
          const sevEl = document.createElement("span");
          sevEl.className = "prism-finding-severity prism-severity-" + sev.replace(/[^a-z]/g, "");
          sevEl.textContent = sev;
          const body = document.createElement("span");
          const strong = document.createElement("strong");
          strong.textContent = locText;
          body.appendChild(strong);
          if (finding.excerpt) {
            body.appendChild(document.createTextNode(`: ${finding.excerpt}`));
          }
          btn.appendChild(sevEl);
          btn.appendChild(body);
          btn.addEventListener("click", () => this.jumpToFinding(finding));
          groupBox.appendChild(btn);
        });

        this.reviewPanel.appendChild(groupBox);
      });
    }

    describeFindingLocation(loc) {
      if (!loc) return "Document";
      if (loc.location_type === "page") return `Page ${(loc.page_index || 0) + 1}`;
      if (loc.location_type === "slide") return `Slide ${(loc.slide_index || 0) + 1}`;
      if (loc.location_type === "sheet") {
        return loc.cell_range ? `${loc.sheet_name}!${loc.cell_range}` : loc.sheet_name;
      }
      if (loc.location_type === "part_path") return loc.path || "Part";
      return "Document";
    }

    jumpToFinding(finding) {
      const loc = finding && finding.location;
      if (!loc) return;
      if (loc.location_type === "page" || loc.location_type === "slide") {
        const pageIdx = loc.page_index ?? loc.slide_index ?? 0;
        this.activeFindingRects = (loc.rects || []).map((r) => ({ page: pageIdx, rect: r }));
        this.goToPage(pageIdx);
        this.renderAllMountedFindings();
      } else if (loc.location_type === "sheet" && loc.sheet_name) {
        const first = this.pageDescriptors.find((d) => d.label === loc.sheet_name);
        if (first) this.goToPage(first.index);
      }
    }

    proposeRedactionsFromSearch() {
      if (this.searchHits.length === 0) return;
      this.searchHits.forEach((hit, idx) => {
        this.annotations.push({
          id: `prop-ui-${idx + 1}`,
          kind: { type: "redaction", reason_code: "SEARCH", label: "REDACTED" },
          page_index: hit.page,
          rects_pct: hit.rects,
          proposed: true,
        });
      });
      const previewWrap = this.root.querySelector("#prism-preview-redact-wrap");
      if (previewWrap) previewWrap.hidden = false;
      this.renderReviewPanelContent();
      this.renderAllMountedAnnotations();
    }

    renderAllMountedFindings() {
      for (const pageIdx of this.mountedPages) {
        this.renderPageFindingOverlays(pageIdx);
      }
    }

    renderPageFindingOverlays(pageIndex) {
      const slot = this.pageSlots[pageIndex];
      if (!slot) return;
      const old = slot.querySelector(".prism-finding-layer");
      if (old) old.remove();

      const desc = this.pageDescriptors[pageIndex];
      const pageW = desc ? desc.widthPt : 612;
      const pageH = desc ? desc.heightPt : 792;

      const rectsToDraw = [];
      for (const item of this.activeFindingRects) {
        if (item.page === pageIndex && item.rect) rectsToDraw.push(item.rect);
      }
      for (const finding of this.findings) {
        if (!this.revealedKinds.has(finding.kind)) continue;
        const loc = finding.location;
        if (!loc) continue;
        const pIdx = loc.page_index ?? loc.slide_index;
        if (pIdx === pageIndex && Array.isArray(loc.rects)) {
          for (const r of loc.rects) rectsToDraw.push(r);
        }
      }

      if (rectsToDraw.length === 0) return;
      const layer = document.createElement("div");
      layer.className = "prism-finding-layer";
      layer.setAttribute("aria-hidden", "true");

      for (const r of rectsToDraw) {
        const box = document.createElement("div");
        box.className = "prism-finding-highlight";
        box.style.left = `${(r.x / pageW) * 100}%`;
        box.style.top = `${(r.y / pageH) * 100}%`;
        box.style.width = `${(r.width / pageW) * 100}%`;
        box.style.height = `${(r.height / pageH) * 100}%`;
        layer.appendChild(box);
      }
      slot.appendChild(layer);
    }

    renderAllMountedAnnotations() {
      for (const pageIdx of this.mountedPages) {
        this.renderPageAnnotations(pageIdx);
      }
    }

    renderPageAnnotations(pageIndex) {
      const slot = this.pageSlots[pageIndex];
      if (!slot) return;
      const old = slot.querySelector(".prism-annotation-layer");
      if (old) old.remove();

      const pageAnns = this.annotations.filter((a) => a.page_index === pageIndex);
      if (pageAnns.length === 0) return;

      const layer = document.createElement("div");
      layer.className = "prism-annotation-layer";
      layer.setAttribute("aria-hidden", "true");

      for (const ann of pageAnns) {
        if (ann.kind && ann.kind.type === "redaction" && !this.previewRedactions) {
          continue;
        }
        const rects = ann.rects_pct || [];
        for (const r of rects) {
          const box = document.createElement("div");
          if (ann.kind && ann.kind.type === "redaction") {
            box.className = ann.proposed
              ? "prism-redaction-box prism-redaction-proposed"
              : "prism-redaction-box";
            box.textContent = ann.kind.label || "REDACTED";
          } else {
            box.className = "prism-annotation-box";
            box.style.background = "rgba(255, 235, 59, 0.4)";
          }
          box.style.left = `${r.x}%`;
          box.style.top = `${r.y}%`;
          box.style.width = `${r.width}%`;
          box.style.height = `${r.height}%`;
          layer.appendChild(box);
        }
      }
      slot.appendChild(layer);
    }

    setupKeyboardShortcuts() {
      window.addEventListener("keydown", (e) => {
        const target = e.target;
        const inInput =
          target &&
          (target.tagName === "INPUT" ||
            target.tagName === "SELECT" ||
            target.tagName === "TEXTAREA");

        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
          const searchInput = this.root.querySelector("#prism-search-input");
          if (searchInput) {
            e.preventDefault();
            searchInput.focus();
            searchInput.select();
          }
          return;
        }

        if (inInput) {
          if (target && target.id === "prism-search-input" && e.key === "Enter") {
            e.preventDefault();
            this.navigateSearchHit(e.shiftKey ? -1 : 1);
          }
          return;
        }

        switch (e.key) {
          case "/": {
            const searchInput = this.root.querySelector("#prism-search-input");
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

  function bootstrapPrismViewer() {
    const roots = document.querySelectorAll(".prism-app-shell");
    if (roots.length > 0) {
      roots.forEach((root) => {
        const ctrl = new PrismViewerController(root);
        void ctrl.init();
      });
    }
  }

  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", bootstrapPrismViewer);
    } else {
      bootstrapPrismViewer();
    }
  }
})();
