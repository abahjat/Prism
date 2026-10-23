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
      this.observer = null;
    }

    async init() {
      await this.discoverPages();
      this.setupToolbar();
      this.setupThumbnails();
      this.setupVirtualization();
      this.setupKeyboardShortcuts();
      this.applyZoomAndRotation();

      window.addEventListener("resize", () => {
        if (this.zoomMode === "fit-width" || this.zoomMode === "fit-page") {
          this.applyZoomAndRotation();
        }
      });
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
          const mode = parseInt(markupSelect.value, 10) || 0;
          await window.PrismHost.setMarkupMode(mode);
          this.root.setAttribute("data-markup-mode", String(mode));
          this.pageDescriptors.forEach((d) => { d.htmlFragment = undefined; });
          for (const idx of Array.from(this.mountedPages)) {
            this.mountedPages.delete(idx);
            await this.mountPage(idx);
          }
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
      if (!trimmed) {
        if (statusEl) statusEl.textContent = "";
        return;
      }

      if (window.PrismHost && window.PrismHost.searchDocument) {
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
