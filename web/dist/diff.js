import { store, toggleSection, alignLines, DIFF_MARKER_RATIO } from "./common.js";

// ======================================
// Diff View
// ======================================
export async function toggleDiff(detailEl, tab, path) {
  const key = `${tab.id}:${path}`;
  if (toggleSection("file", key, detailEl)) {
    detailEl.style.display = "block";
    detailEl.closest(".change-row").classList.add("open");
    await showDiff(detailEl, tab, path);
    if (store.expanded.el === detailEl) {
      scrollToFirstDiffBlock(detailEl);
    }
  } else {
    detailEl.style.display = "none";
    detailEl.textContent = "";
    detailEl.closest(".change-row").classList.remove("open");
  }
}

export function scrollToFirstDiffBlock(detailEl) {
  const first = detailEl.querySelector("tbody.diff-block");
  if (first) {
    scrollDetailToBlock(detailEl, first);
  }
}

// Scrolls only the diff's own scroll box (never the page), centering the
// target block vertically.
export function scrollDetailToBlock(detailEl, block) {
  const dRect = detailEl.getBoundingClientRect();
  const bRect = block.getBoundingClientRect();
  const delta =
    bRect.top - dRect.top - (dRect.height - bRect.height) / 2;
  detailEl.scrollBy({ top: delta, behavior: "smooth" });
}

// Steps between contiguous changed-row groups ("blocks") in an expanded diff.
// The current block is derived from the scroll position so manual scrolling
// never desyncs the ^/v buttons.
export function stepDiffBlock(headEl, delta) {
  const detail = headEl ? headEl.nextElementSibling : null;
  if (!detail || detail.style.display === "none") return;
  const blocks = detail.querySelectorAll("tbody.diff-block");
  if (blocks.length === 0) return;
  const marker = detail.getBoundingClientRect().top + detail.clientHeight * DIFF_MARKER_RATIO;
  let index = 0;
  for (let i = 0; i < blocks.length; i++) {
    if (blocks[i].getBoundingClientRect().top <= marker) index = i;
  }
  const target = Math.max(0, Math.min(index + delta, blocks.length - 1));
  scrollDetailToBlock(detail, blocks[target]);
}

export async function showDiff(detailEl, tab, path) {
  const key = `${tab.id}:${path}`;
  const cached = store.pairCache.get(key);
  if (cached) {
    renderFilePair(detailEl, cached);
    return;
  }
  detailEl.textContent = "Loading...";
  try {
    const response = await fetch(`/files?tab=${tab.id}&path=${encodeURIComponent(path)}`);
    const pair = await response.json();
    store.pairCache.set(key, pair);
    renderFilePair(detailEl, pair);
  } catch (err) {
    detailEl.textContent = "Failed to load diff: " + err;
  }
}

//#region Side-by-side diff rendering

export function renderFilePair(detailEl, pair) {
  detailEl.textContent = "";
  if (pair.original === pair.current) {
    // Contents are identical: the change is mode/permission-only.
    detailEl.textContent =
      "No content changes — this file was changed by permissions or metadata only.";
    return;
  }
  if (pair.original.includes("\u0000") || pair.current.includes("\u0000")) {
    detailEl.textContent = "Binary file — no text diff available.";
    return;
  }
  detailEl.appendChild(renderSideBySide(pair.original, pair.current));
}

export function renderSideBySide(original, current) {
  const rows = alignLines(original, current);

  const table = document.createElement("table");
  table.className = "side-by-side";

  // Contiguous runs of changed rows are grouped into their own tbody so the
  // ^/v buttons can scroll block by block.
  let tbody = document.createElement("tbody");
  let inBlock = false;
  for (const row of rows) {
    const changed =
      (row.left && row.left.type !== "same") ||
      (row.right && row.right.type !== "same");
    if (changed !== inBlock) {
      table.appendChild(tbody);
      tbody = document.createElement("tbody");
      if (changed) {
        tbody.className = "diff-block";
      }
      inBlock = changed;
    }
    const tr = document.createElement("tr");
    for (const side of ["left", "right"]) {
      const cell = document.createElement("td");
      const line = row[side];
      if (line) {
        cell.className = `line-${line.type}`;
        const num = document.createElement("span");
        num.className = "line-num";
        num.textContent = line.num;
        const text = document.createElement("span");
        text.className = "line-text";
        text.textContent = line.text;
        cell.appendChild(num);
        cell.appendChild(text);
      }
      tr.appendChild(cell);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

//#endregion
