// ======================================
// Settings page: LAN access for grit, folio and krust
// ======================================
// The three services each keep a `host` in their own config.json under
// ~/.config/bitshift/<app>/; this page is the one place that writes them. Grit
// applies its own toggle immediately (it owns its listener), while folio and
// krust only read their config at startup and therefore need the restart button.

const SERVICES = ["grit", "folio", "krust"];
const LABELS = { grit: "Grit", folio: "Folio", krust: "Krust" };
const ROLE = {
  grit: "the web UI and git actions",
  folio: "the files dock",
  krust: "the terminal (a shell)",
};
const RESTARTABLE = ["krust", "folio"];

const NOTE =
  "LAN access lets a service answer machines other than this one. Krust is a " +
  "shell and folio can read every file this user can, so turn these on only on " +
  "a network you trust.";

let settings = null;
let busy = false;
let restartMessage = "";

async function fetchJson(url, options) {
  const response = await fetch(url, options);
  if (!response.ok) throw new Error(`${url} responded ${response.status}`);
  return response.json();
}

async function postJson(url, body) {
  return fetchJson(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function serviceRow(key) {
  const service = settings[key];

  const row = document.createElement("div");
  row.className = "settings-service";

  const toggle = document.createElement("input");
  toggle.type = "checkbox";
  toggle.id = `settings-${key}`;
  toggle.checked = service.lan;
  toggle.disabled = busy;
  toggle.addEventListener("change", () => setService(key, toggle.checked));

  const name = document.createElement("label");
  name.className = "settings-name";
  name.htmlFor = toggle.id;
  name.append(toggle, document.createTextNode(LABELS[key]));

  const detail = document.createElement("span");
  detail.className = "settings-detail";
  const port = settings.ports[key];
  detail.textContent = `${service.host}:${port} — ${ROLE[key]}`;
  if (service.effective_host && service.effective_host !== service.host) {
    detail.appendChild(warn(` — listening on ${service.effective_host}`));
  }
  if (service.error) {
    detail.appendChild(warn(` — ${service.error}`));
  }
  if (service.pending_restart) {
    detail.appendChild(warn(" — restart needed"));
  }

  row.append(name, detail);
  return row;
}

function warn(text) {
  const span = document.createElement("span");
  span.className = "settings-warn";
  span.textContent = text;
  return span;
}

function renderSettings() {
  const container = document.getElementById("settings-services");
  if (!container) return;
  container.textContent = "";

  if (!settings) {
    const empty = document.createElement("p");
    empty.className = "muted";
    empty.textContent = "Could not load settings.";
    container.appendChild(empty);
  } else {
    for (const key of SERVICES) container.appendChild(serviceRow(key));
  }

  const restart = document.getElementById("settings-restart-btn");
  const pending =
    settings !== null && RESTARTABLE.some((key) => settings[key].pending_restart);
  restart.style.display = pending ? "" : "none";
  restart.disabled = busy;
  document.getElementById("settings-restart-result").textContent = restartMessage;
}

async function setService(key, lan) {
  busy = true;
  restartMessage = "";
  renderSettings();
  try {
    settings = await postJson("/settings", { [key]: lan });
  } catch (error) {
    console.error("failed to update settings", error);
  }
  busy = false;
  renderSettings();
}

async function restartServices() {
  busy = true;
  restartMessage = "Restarting…";
  renderSettings();
  try {
    const result = await postJson("/settings/restart", {});
    const failed = Object.entries(result).filter(([, value]) => !value.ok);
    restartMessage = failed.length
      ? `Failed: ${failed.map(([key, value]) => `${key} (${value.error})`).join(", ")}`
      : "Restarted krust and folio.";
  } catch (error) {
    restartMessage = `Restart failed: ${error.message}`;
  }
  busy = false;
  // Refresh from the server, then keep the message on screen.
  await loadSettings();
}

/** Fetches the current settings and paints them. */
export async function loadSettings() {
  try {
    settings = await fetchJson("/settings", { headers: { accept: "application/json" } });
  } catch (error) {
    console.error("failed to load settings", error);
    settings = null;
  }
  renderSettings();
}

/** Wires the note and the restart button; called once from app.js. */
export function initSettings() {
  const note = document.getElementById("settings-note");
  if (note) note.textContent = NOTE;
  const restart = document.getElementById("settings-restart-btn");
  if (restart) restart.addEventListener("click", restartServices);
}
