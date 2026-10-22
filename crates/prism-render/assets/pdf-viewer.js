// SPDX-License-Identifier: AGPL-3.0-only
// Prism offline PDF viewer bootstrap.
//
// pdf.js and its worker are embedded in the page as inert text blocks and
// loaded through blob: URLs, so rendering needs no network access. Pages are
// rendered lazily as they scroll into view.
const blobUrl = (id) =>
  URL.createObjectURL(
    new Blob([document.getElementById(id).textContent], { type: "text/javascript" })
  );

const viewers = document.querySelectorAll(".pdf-viewer");
const reportFailure = (err) => {
  for (const viewer of viewers) {
    viewer.querySelector(".pdf-status").textContent =
      `Unable to load the PDF viewer: ${err && err.message ? err.message : err}`;
  }
};

let pdfjsLib;
try {
  pdfjsLib = await import(blobUrl("prism-pdfjs-lib"));
  pdfjsLib.GlobalWorkerOptions.workerSrc = blobUrl("prism-pdfjs-worker");
} catch (err) {
  reportFailure(err);
  throw err;
}

const decodeBase64 = (b64) => {
  const binary = atob(b64.trim());
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
};

async function mountViewer(viewer) {
  const status = viewer.querySelector(".pdf-status");
  const pagesEl = viewer.querySelector(".pdf-pages");
  const payload = viewer.querySelector('script[type="application/pdf+base64"]');
  let pdf;
  try {
    pdf = await pdfjsLib.getDocument({
      data: decodeBase64(payload.textContent),
      enableXfa: false,
      stopAtErrors: false,
    }).promise;
  } catch (err) {
    status.textContent = `Unable to render PDF: ${err && err.message ? err.message : err}`;
    return;
  }
  payload.remove();
  status.textContent = `${pdf.numPages} page${pdf.numPages === 1 ? "" : "s"}`;

  const first = await pdf.getPage(1);
  const base = first.getViewport({ scale: 1 });
  const rendered = new Set();

  const render = async (pageEl) => {
    const number = Number(pageEl.dataset.page);
    if (rendered.has(number)) return;
    rendered.add(number);
    const page = await pdf.getPage(number);
    const unscaled = page.getViewport({ scale: 1 });
    const scale = Math.min(pagesEl.clientWidth || unscaled.width, 1400) / unscaled.width;
    const viewport = page.getViewport({ scale });
    const ratio = window.devicePixelRatio || 1;
    const canvas = document.createElement("canvas");
    canvas.width = Math.floor(viewport.width * ratio);
    canvas.height = Math.floor(viewport.height * ratio);
    canvas.style.width = `${Math.floor(viewport.width)}px`;
    canvas.style.height = `${Math.floor(viewport.height)}px`;
    pageEl.style.aspectRatio = "";
    pageEl.replaceChildren(canvas);
    await page.render({
      canvasContext: canvas.getContext("2d"),
      viewport,
      transform: ratio === 1 ? null : [ratio, 0, 0, ratio, 0, 0],
    }).promise;
  };

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          observer.unobserve(entry.target);
          render(entry.target).catch((err) => {
            entry.target.textContent = `Page ${entry.target.dataset.page} failed to render: ${err}`;
          });
        }
      }
    },
    { rootMargin: "400px 0px" }
  );

  for (let n = 1; n <= pdf.numPages; n++) {
    const pageEl = document.createElement("div");
    pageEl.className = "pdf-page";
    pageEl.dataset.page = String(n);
    pageEl.style.aspectRatio = `${base.width} / ${base.height}`;
    pageEl.setAttribute("aria-label", `Page ${n}`);
    pagesEl.append(pageEl);
    observer.observe(pageEl);
  }
}

for (const viewer of viewers) {
  mountViewer(viewer).catch((err) => {
    viewer.querySelector(".pdf-status").textContent = `Unable to render PDF: ${err}`;
  });
}
