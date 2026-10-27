/* ============================================================
   Mirr frontend logic.
   Dual-mode: under Tauri it drives the Rust backend (invoke + events)
   and docks the native mirror video into #videoHost. In a plain
   browser it falls back to demo data so the UI stays explorable.
   No build step, no imports.
   ============================================================ */
"use strict";

const TAURI = !!(window.__TAURI__ && window.__TAURI__.core);
const invoke = TAURI ? window.__TAURI__.core.invoke : null;
const listen = TAURI && window.__TAURI__.event ? window.__TAURI__.event.listen : null;

/* keep in sync with --screen-radius in styles.css */
const SCREEN_RADIUS = 20;
const ZOOM_MIN = 100, ZOOM_MAX = 300, ZOOM_STEP = 25;

const $ = (id) => document.getElementById(id) || document.querySelector(`[data-alias="${id}"]`);
$("envtag").textContent = TAURI ? "Live · Tauri" : "Demo mode — run the app for the real thing";
if (TAURI) document.body.classList.add("tauri");

/* ---------- frameless window controls ---------- */
if (TAURI && window.__TAURI__.window) {
  const win = window.__TAURI__.window.getCurrentWindow();
  $("winctl").hidden = false;
  $("wmin").addEventListener("click", () => win.minimize());
  $("wmax").addEventListener("click", () => win.toggleMaximize());
  $("wclose").addEventListener("click", () => win.close());
}

/* ---------- toasts ---------- */
function toast(msg, bad = false, actionLabel = "", onAction = null) {
  const el = document.createElement("div");
  el.className = "toast" + (bad ? " bad" : "");
  const span = document.createElement("span");
  span.textContent = msg;
  el.appendChild(span);
  if (actionLabel && typeof onAction === "function") {
    const act = document.createElement("button");
    act.className = "toast-act";
    act.textContent = actionLabel;
    act.addEventListener("click", () => {
      onAction();
      el.remove();
    });
    el.appendChild(act);
  }
  $("toasts").appendChild(el);
  const dur = actionLabel ? 5200 : 3600;
  setTimeout(() => { el.style.opacity = "0"; el.style.transition = "opacity .4s"; }, dur);
  setTimeout(() => el.remove(), dur + 500);
}

/* ---------- runtime settings state ---------- */
let currentBroadcastName = "Mirr";
let runtimeSettings = {
  broadcastName: "Mirr",
  fps: 60,
  vsync: false,
  alwaysOnTopMirror: false,
  autoTheater: false,
  audioOutputDevice: "default",
  autoSwitchAudioOutput: true,
  preferDolbyAtmos: true,
  bgTheme: "horizon",
  // REMINDER: Default showTransportControls to false on initial setup of the app
  // as requested until previous/next song and pause/play controls are fully resolved.
  showTransportControls: false,
  receiverMode: "dual"
};
let pipMode = false;

function applyTheme(themeKey) {
  const valid = ["horizon", "graphite", "ink", "espresso", "grid", "acrylic", "aurora"];
  const t = valid.includes(themeKey) ? themeKey : "horizon";
  runtimeSettings.bgTheme = t;
  const bg = document.querySelector(".bg");
  if (bg) {
    bg.className = "bg theme-" + t;
  }
  document.body.classList.toggle("theme-acrylic", t === "acrylic");
  document.querySelectorAll("#themeGrid .theme-card").forEach((card) => {
    card.classList.toggle("active", card.dataset.theme === t);
  });
}

let currentAccentStyle = {
  type: "color",
  value: "#A470E0",
  name: "Copic FV2 · Violet"
};

function applyAccentStyle(styleObj, save = true) {
  if (!styleObj || !styleObj.value) return;
  currentAccentStyle = styleObj;

  const root = document.documentElement;
  if (styleObj.type === "color") {
    root.style.setProperty("--accent", styleObj.value);
    root.style.setProperty("--accent-fill", styleObj.value);
    root.style.setProperty("--accent2", styleObj.value);
  } else {
    root.style.setProperty("--accent-fill", styleObj.value);
    if (styleObj.stops && styleObj.stops.length > 0) {
      root.style.setProperty("--accent", styleObj.stops[0].color);
      if (styleObj.stops.length > 1) {
        root.style.setProperty("--accent2", styleObj.stops[styleObj.stops.length - 1].color);
      }
    } else {
      const hexes = styleObj.value.match(/#[0-9A-Fa-f]{6}/g);
      if (hexes && hexes.length > 0) {
        root.style.setProperty("--accent", hexes[0]);
        if (hexes.length > 1) {
          root.style.setProperty("--accent2", hexes[hexes.length - 1]);
        }
      } else {
        root.style.setProperty("--accent", "#A470E0");
      }
    }
  }

  document.querySelectorAll(".accent-chip").forEach((chip) => {
    const isMatch = (chip.dataset.val || "").toLowerCase().trim() === styleObj.value.toLowerCase().trim();
    chip.classList.toggle("active", isMatch);
  });

  if ($("builderPreviewPlay")) $("builderPreviewPlay").style.background = styleObj.value;
  if ($("builderPreviewBar")) $("builderPreviewBar").style.background = styleObj.value;
  if ($("builderPreviewWave")) {
    $("builderPreviewWave").querySelectorAll("i").forEach(el => el.style.background = styleObj.value);
  }
  if ($("builderCssCode")) $("builderCssCode").textContent = styleObj.value;

  if (save) {
    try {
      localStorage.setItem("mirr_accent_style", JSON.stringify(styleObj));
    } catch (_) {}
  }
}

const gradientBuilderState = {
  angle: 135,
  stops: [
    { color: "#A470E0", pos: 0 },
    { color: "#6C5CE7", pos: 100 }
  ]
};

function getBuilderGradientCss() {
  const sortedStops = [...gradientBuilderState.stops].sort((a, b) => a.pos - b.pos);
  const stopsStr = sortedStops.map(s => `${s.color} ${s.pos}%`).join(", ");
  return `linear-gradient(${gradientBuilderState.angle}deg, ${stopsStr})`;
}

function updateBuilderPreview() {
  const css = getBuilderGradientCss();
  if ($("builderPreviewPlay")) $("builderPreviewPlay").style.background = css;
  if ($("builderPreviewBar")) $("builderPreviewBar").style.background = css;
  if ($("builderPreviewWave")) {
    $("builderPreviewWave").querySelectorAll("i").forEach(el => el.style.background = css);
  }
  if ($("builderCssCode")) $("builderCssCode").textContent = css;
}

function renderBuilderStops() {
  const list = $("builderStopsList");
  if (!list) return;
  list.innerHTML = "";

  gradientBuilderState.stops.forEach((stop, idx) => {
    const row = document.createElement("div");
    row.className = "stop-row";
    row.innerHTML = `
      <input type="color" value="${stop.color}" class="stop-color-input" aria-label="Stop color">
      <input type="text" value="${stop.color}" class="stop-hex-input" spellcheck="false" maxlength="9">
      <input type="range" min="0" max="100" value="${stop.pos}" class="stop-pos-slider" aria-label="Stop position">
      <span class="stop-pos-val">${stop.pos}%</span>
      ${gradientBuilderState.stops.length > 2 ? `<button type="button" class="stop-remove-btn" title="Remove stop">✕</button>` : ""}
    `;

    const colorInput = row.querySelector(".stop-color-input");
    const hexInput = row.querySelector(".stop-hex-input");
    const posInput = row.querySelector(".stop-pos-slider");
    const posVal = row.querySelector(".stop-pos-val");
    const removeBtn = row.querySelector(".stop-remove-btn");

    colorInput.addEventListener("input", (e) => {
      stop.color = e.target.value;
      hexInput.value = e.target.value;
      updateBuilderPreview();
    });

    hexInput.addEventListener("input", (e) => {
      let val = e.target.value.trim();
      if (!val.startsWith("#")) val = "#" + val;
      if (/^#[0-9A-Fa-f]{6}$/.test(val)) {
        stop.color = val;
        colorInput.value = val;
        updateBuilderPreview();
      }
    });

    posInput.addEventListener("input", (e) => {
      stop.pos = Number(e.target.value);
      posVal.textContent = stop.pos + "%";
      updateBuilderPreview();
    });

    if (removeBtn) {
      removeBtn.addEventListener("click", () => {
        gradientBuilderState.stops.splice(idx, 1);
        renderBuilderStops();
        updateBuilderPreview();
      });
    }

    list.appendChild(row);
  });
}

function initAccentControls() {
  document.querySelectorAll(".accent-chip").forEach((chip) => {
    const pick = () => {
      const type = chip.dataset.type;
      const val = chip.dataset.val;
      const name = chip.querySelector(".chip-name")?.textContent || val;
      applyAccentStyle({ type, value: val, name });
      if (type === "gradient") {
        const hexes = val.match(/#[0-9A-Fa-f]{6}/g);
        if (hexes && hexes.length >= 2) {
          gradientBuilderState.stops = hexes.map((hex, i) => ({
            color: hex,
            pos: Math.round((i / (hexes.length - 1)) * 100)
          }));
          renderBuilderStops();
          updateBuilderPreview();
        }
      } else if (type === "color") {
        if ($("builderSolidColorInput")) $("builderSolidColorInput").value = val;
        if ($("builderSolidHexInput")) $("builderSolidHexInput").value = val;
      }
      toast(`Accent set to ${name}`);
    };
    chip.addEventListener("click", pick);
  });

  if ($("builderAngleSlider")) {
    $("builderAngleSlider").addEventListener("input", (e) => {
      gradientBuilderState.angle = Number(e.target.value);
      if ($("builderAngleVal")) $("builderAngleVal").textContent = e.target.value + "°";
      updateBuilderPreview();
    });
  }

  document.querySelectorAll(".angle-quick-btns button").forEach((btn) => {
    btn.addEventListener("click", () => {
      const a = Number(btn.dataset.angle);
      gradientBuilderState.angle = a;
      if ($("builderAngleSlider")) $("builderAngleSlider").value = a;
      if ($("builderAngleVal")) $("builderAngleVal").textContent = a + "°";
      updateBuilderPreview();
    });
  });

  if ($("builderAddStopBtn")) {
    $("builderAddStopBtn").addEventListener("click", () => {
      if (gradientBuilderState.stops.length >= 6) return;
      const last = gradientBuilderState.stops[gradientBuilderState.stops.length - 1];
      const newPos = Math.min(100, Math.round(last.pos + 20));
      gradientBuilderState.stops.push({ color: "#00C6FF", pos: newPos });
      renderBuilderStops();
      updateBuilderPreview();
    });
  }

  if ($("builderApplyGradientBtn")) {
    $("builderApplyGradientBtn").addEventListener("click", () => {
      const css = getBuilderGradientCss();
      applyAccentStyle({
        type: "gradient",
        value: css,
        stops: [...gradientBuilderState.stops],
        angle: gradientBuilderState.angle,
        name: "Custom Gradient"
      });
      toast("Custom gradient applied!");
    });
  }

  if ($("builderSolidColorInput") && $("builderSolidHexInput")) {
    $("builderSolidColorInput").addEventListener("input", (e) => {
      $("builderSolidHexInput").value = e.target.value;
    });
    $("builderSolidHexInput").addEventListener("input", (e) => {
      let val = e.target.value.trim();
      if (!val.startsWith("#")) val = "#" + val;
      if (/^#[0-9A-Fa-f]{6}$/.test(val)) {
        $("builderSolidColorInput").value = val;
      }
    });
  }

  if ($("builderApplySolidBtn")) {
    $("builderApplySolidBtn").addEventListener("click", () => {
      let hex = ($("builderSolidHexInput")?.value || "#A470E0").trim();
      if (!hex.startsWith("#")) hex = "#" + hex;
      if (!/^#[0-9A-Fa-f]{6}$/.test(hex)) {
        toast("Please enter a valid 6-digit hex color (e.g. #A470E0)", true);
        return;
      }
      applyAccentStyle({ type: "color", value: hex, name: "Custom Solid" });
      toast("Custom solid color applied: " + hex);
    });
  }

  if ($("builderResetBtn")) {
    $("builderResetBtn").addEventListener("click", () => {
      applyAccentStyle({ type: "color", value: "#A470E0", name: "Copic FV2 · Violet" });
      if ($("builderSolidColorInput")) $("builderSolidColorInput").value = "#A470E0";
      if ($("builderSolidHexInput")) $("builderSolidHexInput").value = "#A470E0";
      toast("Accent reset to Copic FV2");
    });
  }

  renderBuilderStops();
  updateBuilderPreview();

  try {
    const savedAccent = JSON.parse(localStorage.getItem("mirr_accent_style") || "null");
    if (savedAccent && savedAccent.value) {
      applyAccentStyle(savedAccent, false);
      if (savedAccent.type === "gradient" && Array.isArray(savedAccent.stops) && savedAccent.stops.length >= 2) {
        gradientBuilderState.stops = savedAccent.stops;
        if (typeof savedAccent.angle === "number") {
          gradientBuilderState.angle = savedAccent.angle;
          if ($("builderAngleSlider")) $("builderAngleSlider").value = savedAccent.angle;
          if ($("builderAngleVal")) $("builderAngleVal").textContent = savedAccent.angle + "°";
        }
        renderBuilderStops();
        updateBuilderPreview();
      } else if (savedAccent.type === "color") {
        if ($("builderSolidColorInput")) $("builderSolidColorInput").value = savedAccent.value;
        if ($("builderSolidHexInput")) $("builderSolidHexInput").value = savedAccent.value;
      }
    } else {
      applyAccentStyle({ type: "color", value: "#A470E0", name: "Copic FV2 · Violet" }, false);
    }
  } catch (_) {
    applyAccentStyle({ type: "color", value: "#A470E0", name: "Copic FV2 · Violet" }, false);
  }
}

try {
  const initCached = JSON.parse(localStorage.getItem("mirr_settings") || "null");
  if (initCached && initCached.bgTheme) {
    runtimeSettings.bgTheme = initCached.bgTheme;
  }
} catch (_) {}
applyTheme(runtimeSettings.bgTheme || "horizon");
initAccentControls();

/* ---------- navigation ---------- */
const titles = {
  receive: ["AirPlay Receiver", "Your PC appears as “Mirr”"],
  airplay2: ["AirPlay 2 Audio", "Lossless multi-room receiver"],
  cast: ["AirCast", "Send this PC’s audio to AirPlay speakers"],
  localsend: ["LocalSend", "Cross-platform transfer over your local network"],
  settings: ["Settings", "Identity, capabilities and honest feasibility"]
};
function nav(btn) {
  document.querySelectorAll(".navbtn").forEach(b => b.classList.remove("active"));
  btn.classList.add("active");
  const v = btn.dataset.view;
  document.querySelectorAll(".view").forEach(el => el.classList.remove("show"));
  const view = $(v);
  void view.offsetWidth; view.classList.add("show");
  $("vtitle").textContent = titles[v][0];
  $("vsub").textContent = v === "receive" ? `Your PC appears as “${currentBroadcastName}”` : titles[v][1];
  // the docked mirror video is a native window — hide it whenever the
  // receive stage isn't the active pane
  if (TAURI) invoke("set_video_visible", { visible: v === "receive" }).catch(() => {});
  if (v === "receive") reportRectSoon();
}
document.querySelectorAll(".navbtn").forEach(b => b.addEventListener("click", () => nav(b)));

/* ---------- icons ---------- */
const ICON = {
  iphone: '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.7"><rect x="6" y="2" width="12" height="20" rx="3"/><path d="M11 19h2"/></svg>',
  mac: '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.7"><rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/></svg>',
  ipad: '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.7"><rect x="4" y="3" width="16" height="18" rx="3"/></svg>',
  speaker: '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.7"><rect x="6" y="2" width="12" height="20" rx="4"/><circle cx="12" cy="15" r="3"/></svg>',
  android: '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.6"><rect x="7" y="2" width="10" height="20" rx="2"/></svg>',
  generic: '<svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.6"><rect x="3" y="5" width="18" height="14" rx="2"/></svg>'
};
function pickIcon(t) {
  t = (t || "").toLowerCase();
  if (t.includes("iphone")) return ICON.iphone;
  if (t.includes("ipad")) return ICON.ipad;
  if (t.includes("mac")) return ICON.mac;
  if (t.includes("android") || t.includes("pixel") || t.includes("galaxy")) return ICON.android;
  if (t.includes("speaker") || t.includes("homepod") || t.includes("express")) return ICON.speaker;
  return ICON.generic;
}
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

/* ---------- demo data (browser only) ---------- */
const DEMO = {
  airplayDevices: [],
  castTargets: [
    { name: "Living Room HomePod", meta: "AirPlay 2 · paired", on: true, vol: 82 },
    { name: "Kitchen — AirPort Express", meta: "Classic AirPlay · RAOP", on: false, vol: 55 },
    { name: "Office Speakers", meta: "AirPlay 2 · grouped", on: false, vol: 40 }
  ],
  localsendPeers: []
};

/* ============================================================
   AirPlay receive — engine state machine + native video docking
   ============================================================ */
let engineState = "starting"; // starting | ready | connecting | mirroring | stopped | error
let engineDetail = ""; // last detail string, so a repaint can't lose an error message
let mirroring = false;
let zoom = 100;
let windowFocused = document.hasFocus();
/* declared up here, not next to goActive(): loadDemo()/loadFromBackend() call
   applyEngineState -> goActive() during load, which would hit the temporal dead
   zone if these lived further down the file. */
const CALM_AFTER_MS = 12000;
let calmTimer = null;
let lastCalmReset = 0;

/* ---------- audio-only Now Playing session ---------- */
let npActive = false;
let dacpAvailable = false;
let npMeta = { title: "", artist: "", album: "", hasData: false, rev: -1 };
// local 1 Hz elapsed-time ticker: extrapolated from the last progress
// baseline using the wall clock, re-baselined on every progress event
const np = { baseSec: null, durSec: null, baseWall: 0, playing: false };
// demo playlist used for explore mode and fallback stepping with embedded art
const DEMO_PLAYLIST = [
  {
    title: "Resonance — Ambient Mix",
    artist: "Aurora Fields",
    album: "Night Drift",
    duration: 222,
    art: 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="400" height="400" viewBox="0 0 400 400"><defs><linearGradient id="g1" x1="0%" y1="0%" x2="100%" y2="100%"><stop offset="0%" stop-color="%237db2ff"/><stop offset="50%" stop-color="%23c894ff"/><stop offset="100%" stop-color="%23ff8ec0"/></linearGradient><radialGradient id="r1" cx="30%" cy="30%" r="70%"><stop offset="0%" stop-color="%23ffffff" stop-opacity="0.35"/><stop offset="100%" stop-color="%23000000" stop-opacity="0.4"/></radialGradient></defs><rect width="400" height="400" rx="36" fill="url(%23g1)"/><rect width="400" height="400" rx="36" fill="url(%23r1)"/><circle cx="200" cy="200" r="90" fill="none" stroke="%23ffffff" stroke-width="5" stroke-opacity="0.6"/><path d="M165 140v120l95-60z" fill="%23ffffff" fill-opacity="0.9"/></svg>'
  },
  {
    title: "Solaris (Extended)",
    artist: "Komorebi",
    album: "Atmospheres Vol. 2",
    duration: 198,
    art: 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="400" height="400" viewBox="0 0 400 400"><defs><linearGradient id="g2" x1="0%" y1="0%" x2="100%" y2="100%"><stop offset="0%" stop-color="%23ff9a44"/><stop offset="50%" stop-color="%23fc6076"/><stop offset="100%" stop-color="%237a42f4"/></linearGradient><radialGradient id="r2" cx="70%" cy="30%" r="70%"><stop offset="0%" stop-color="%23ffffff" stop-opacity="0.3"/><stop offset="100%" stop-color="%23000000" stop-opacity="0.45"/></radialGradient></defs><rect width="400" height="400" rx="36" fill="url(%23g2)"/><rect width="400" height="400" rx="36" fill="url(%23r2)"/><circle cx="200" cy="200" r="100" fill="%23ffffff" fill-opacity="0.15"/><circle cx="200" cy="200" r="60" fill="%23ffffff" fill-opacity="0.25"/></svg>'
  },
  {
    title: "Midnight City Lights",
    artist: "Vapor Wave",
    album: "Neon Horizons",
    duration: 245,
    art: 'data:image/svg+xml;utf8,<svg xmlns="http://www.w3.org/2000/svg" width="400" height="400" viewBox="0 0 400 400"><defs><linearGradient id="g3" x1="0%" y1="100%" x2="100%" y2="0%"><stop offset="0%" stop-color="%2300f2fe"/><stop offset="50%" stop-color="%234facfe"/><stop offset="100%" stop-color="%236b11ff"/></linearGradient><radialGradient id="r3" cx="50%" cy="50%" r="70%"><stop offset="0%" stop-color="%23ffffff" stop-opacity="0.25"/><stop offset="100%" stop-color="%23000000" stop-opacity="0.5"/></radialGradient></defs><rect width="400" height="400" rx="36" fill="url(%23g3)"/><rect width="400" height="400" rx="36" fill="url(%23r3)"/><polygon points="200,110 280,270 120,270" fill="none" stroke="%2300f2fe" stroke-width="6" stroke-opacity="0.8"/></svg>'
  }
];
let demoTrackIdx = 0;
const DEMO_TRACK = DEMO_PLAYLIST[0];
let isDemoAudioActive = false;

function setDisco(txt, dotCls) {
  $("disco").textContent = txt;
  $("discoDot").className = "dot " + dotCls;
}

function updatePipTitle() {
  const el = $("pipTitle");
  if (!el) return;
  if (mirroring) el.textContent = `Mirroring · ${currentBroadcastName}`;
  else if (npActive) el.textContent = (npMeta.hasData && npMeta.title) ? npMeta.title : "Audio Session";
  else el.textContent = `Mirr · ${currentBroadcastName}`;
}

function applyEngineState(state, detail) {
  if (state !== engineState && typeof goActive === "function") goActive();
  engineState = state;
  engineDetail = detail || "";
  const title = $("rxTitle"), sub = $("rxSub"), retry = $("rxRetry"), st = $("rxState");
  retry.hidden = true;
  switch (state) {
    case "starting":
      title.textContent = "Starting receiver…";
      sub.textContent = "Booting the AirPlay engine";
      st.textContent = "Starting…";
      setDisco("Starting…", "warn pulse");
      break;
    case "ready":
      title.textContent = "Ready for AirPlay";
      sub.innerHTML = `On your iPhone or Mac, open <b>Screen Mirroring</b> and pick <b>${esc(currentBroadcastName)}</b>`;
      st.textContent = "Idle — waiting for a device";
      setDisco("Discoverable as " + currentBroadcastName, "pulse");
      break;
    case "connecting":
      title.textContent = "Device connecting…";
      sub.textContent = "Negotiating the AirPlay session";
      st.textContent = "Connecting…";
      setDisco("Connecting…", "warn pulse");
      break;
    case "mirroring":
      st.textContent = "Mirroring live — video and audio";
      setDisco("Mirroring", "pulse");
      break;
    case "stopped":
      title.textContent = "Receiver stopped";
      sub.textContent = "The AirPlay engine is not running";
      st.textContent = "Stopped";
      retry.hidden = false;
      setDisco("Offline", "off");
      break;
    case "error":
      title.textContent = "Receiver unavailable";
      sub.textContent = detail || "The AirPlay engine failed to start";
      st.textContent = "Error";
      retry.hidden = false;
      setDisco("Error", "bad");
      break;
  }
  syncRxButton();
  updatePipTitle();
}

function syncRxButton() {
  if (!TAURI) return;
  rxBtn.textContent = (engineState === "stopped" || engineState === "error") ? "Start receiver" : "Stop receiver";
}

/* ---------- Now Playing stage: pure function of (mirroring, npActive) ----------
   Mirror always wins. Drives body.nowplaying, npStage visibility, rxEmpty
   visibility (jointly with mirroring), the transport clusters, and the
   rxState/topbar copy while an audio session owns the stage. */
function renderStage() {
  const showNp = !mirroring && npActive;
  document.body.classList.toggle("nowplaying", showNp);
  $("npStage").hidden = !showNp;
  $("rxEmpty").classList.toggle("hide", mirroring || showNp);
  if ($("npTransport")) $("npTransport").hidden = false;
  if ($("npVolWrap")) $("npVolWrap").hidden = false;
  if ($("mTransport")) {
    $("mTransport").hidden = !mirroring;
    if ($("mVolWrap")) $("mVolWrap").hidden = false;
  }
  updateTransportControlsVisibility();
  if (showNp) {
    const title = npMeta.hasData && npMeta.title ? npMeta.title : "Audio session live";
    $("rxState").textContent = "Audio session · " + title;
    setDisco("Audio session", "pulse");
    updatePipTitle();
  } else {
    // the audio stage no longer owns the copy — hand the pill/label back to
    // the engine state (never recurses: applyEngineState doesn't call us)
    applyEngineState(engineState, engineDetail);
  }
}

function fmtTime(s) {
  s = Math.max(0, Math.floor(s));
  const m = Math.floor(s / 60), sec = s % 60;
  return m + ":" + String(sec).padStart(2, "0");
}

/* indeterminate <-> ticking <-> frozen, per the local 1 Hz ticker */
function resetNpProgress() {
  np.baseSec = null; np.durSec = null; np.baseWall = 0; np.playing = false;
  updatePlayIcon(false);
  $("npBarWrap").classList.add("busy");
  $("npBar").style.width = "";
  $("npElapsed").textContent = "–:––";
  $("npRemain").textContent = "–:––";
}

function renderNpProgress() {
  if (!npActive || np.baseSec == null || !np.durSec) {
    $("npBarWrap").classList.add("busy");
    return;
  }
  let elapsed = np.playing ? np.baseSec + (Date.now() - np.baseWall) / 1000 : np.baseSec;
  elapsed = Math.max(0, Math.min(elapsed, np.durSec));
  // clamp-freeze at duration (guarded so the icon repaints once, not per tick)
  if (np.playing && elapsed >= np.durSec) { np.playing = false; updatePlayIcon(false); }
  $("npBarWrap").classList.remove("busy");
  $("npElapsed").textContent = fmtTime(elapsed);
  $("npRemain").textContent = "−" + fmtTime(np.durSec - elapsed);
  $("npBar").style.width = (np.durSec > 0 ? (elapsed / np.durSec * 100) : 0) + "%";
}
// only ticks while an audio session is actively playing and visible — no idle repaints
setInterval(() => { if (npActive && np.playing && !document.hidden) renderNpProgress(); }, 1000);

/* re-baseline on every nowplaying://progress event (and on late-attach
   rehydration, where ageMs backdates baseWall) */
function setNpProgress(position, duration, ageMs) {
  np.baseSec = position;
  np.durSec = duration;
  np.baseWall = Date.now() - (ageMs || 0);
  np.playing = true;
  updatePlayIcon(true);
  renderNpProgress();
}

function setNpArt(dataUrl) {
  const art = $("npArt"), bg = $("npBg"), img = $("npArtImg");
  if (dataUrl && typeof dataUrl === "string" && dataUrl.trim().length > 0) {
    if (img) {
      img.onerror = () => {
        img.hidden = true;
        if (art && !art.style.backgroundImage) art.classList.remove("has-art");
      };
      img.onload = () => {
        img.hidden = false;
        if (art) art.classList.add("has-art");
      };
      img.src = dataUrl;
      img.hidden = false;
    }
    if (art) {
      art.style.backgroundImage = `url("${dataUrl}")`;
      art.style.backgroundSize = "cover";
      art.style.backgroundPosition = "center center";
      art.style.backgroundRepeat = "no-repeat";
      art.classList.add("has-art");
    }
    if (bg) {
      bg.style.backgroundImage = `url("${dataUrl}")`;
      bg.classList.add("has-art");
    }
  } else {
    if (img) {
      img.removeAttribute("src");
      img.hidden = true;
    }
    if (art) {
      art.style.backgroundImage = "";
      art.classList.remove("has-art");
    }
    if (bg) {
      bg.style.backgroundImage = "";
      bg.classList.remove("has-art");
    }
  }
}

function updatePipTextScroll() {
  const wrap = $("npMetaWrap");
  const scroll = $("npMetaScroll");
  if (!wrap || !scroll) return;
  if (!document.body.classList.contains("pip")) {
    wrap.classList.remove("auto-scroll-y");
    wrap.style.removeProperty("--scroll-y");
    return;
  }
  const diff = scroll.scrollHeight - wrap.clientHeight;
  if (diff > 4) {
    wrap.style.setProperty("--scroll-y", `-${diff + 6}px`);
    wrap.classList.add("auto-scroll-y");
  } else {
    wrap.classList.remove("auto-scroll-y");
    wrap.style.removeProperty("--scroll-y");
  }
}

function setNpMeta(meta) {
  meta = meta || {};
  const revChanged = meta.rev !== npMeta.rev;
  npMeta = { title: meta.title || "", artist: meta.artist || "", album: meta.album || "",
    hasData: !!meta.hasData, rev: meta.rev };
  const titleEl = $("npTitle");
  const albumEl = $("npAlbum");
  const artistEl = $("npArtist");

  if (titleEl) {
    titleEl.textContent = npMeta.hasData && npMeta.title ? npMeta.title : "Audio from your device";
    titleEl.title = titleEl.textContent;
  }

  if (albumEl) {
    if (npMeta.hasData && npMeta.album) {
      albumEl.textContent = npMeta.album;
      albumEl.title = npMeta.album;
      albumEl.hidden = false;
    } else {
      albumEl.textContent = "";
      albumEl.hidden = true;
    }
  }

  if (artistEl) {
    let artistLine = "";
    if (npMeta.hasData) {
      artistLine = npMeta.artist || "";
    }
    artistEl.textContent = artistLine || "Waiting for track info…";
    artistEl.title = artistEl.textContent;
  }

  // -md rewrites happen on every SETUP; progress resets when track rev changes
  // and cover art updates whenever nowplaying://art arrives.
  if (revChanged) {
    resetNpProgress();
  }
  renderStage();
  updatePipTextScroll();

  if ('mediaSession' in navigator && window.MediaMetadata) {
    try {
      navigator.mediaSession.metadata = new MediaMetadata({
        title: npMeta.hasData && npMeta.title ? npMeta.title : "Audio from your device",
        artist: npMeta.hasData && npMeta.artist ? npMeta.artist : "Mirr",
        album: npMeta.hasData && npMeta.album ? npMeta.album : ""
      });
    } catch (_) {}
  }
}

function updatePlayIcon(playing) {
  const path = playing
    ? '<path d="M8 6h3v12H8zM13 6h3v12h-3z"/>'
    : '<path d="M8 5v14l11-7Z"/>';
  const label = playing ? "Pause" : "Play";
  ["npPlay", "mPlay", "thPlay", "a2Play"].forEach(id => {
    const el = $(id);
    if (el) {
      el.setAttribute("aria-label", label);
      el.setAttribute("title", label);
      const svg = el.querySelector("svg");
      if (svg) svg.innerHTML = path;
    }
  });
  if ($("npPlayIcon")) $("npPlayIcon").innerHTML = path;
  if ($("mPlayIcon")) $("mPlayIcon").innerHTML = path;
  if ($("thPlayIcon")) $("thPlayIcon").innerHTML = path;
  if ($("a2PlayIcon")) $("a2PlayIcon").innerHTML = path;
  if ('mediaSession' in navigator) {
    try {
      navigator.mediaSession.playbackState = playing ? "playing" : "paused";
    } catch (_) {}
  }
}

function toggleLocalPlayPause() {
  if (np.playing && np.baseSec != null) {
    np.baseSec = Math.min(np.durSec ?? Infinity, np.baseSec + (Date.now() - np.baseWall) / 1000);
  }
  np.playing = !np.playing;
  np.baseWall = Date.now();
  updatePlayIcon(np.playing);
  renderNpProgress();
}

function stepDemoTrack(delta) {
  if (delta < 0 && np.playing && np.baseSec != null) {
    const elapsed = np.baseSec + (Date.now() - np.baseWall) / 1000;
    if (elapsed > 3) {
      setNpProgress(0, DEMO_PLAYLIST[demoTrackIdx].duration, 0);
      updatePlayIcon(true);
      return;
    }
  }
  demoTrackIdx = (demoTrackIdx + delta + DEMO_PLAYLIST.length) % DEMO_PLAYLIST.length;
  const t = DEMO_PLAYLIST[demoTrackIdx];
  setNpMeta({ title: t.title, artist: t.artist, album: t.album, hasData: true, rev: Date.now() });
  setNpArt(t.art || null);
  setNpProgress(0, t.duration, 0);
  updatePlayIcon(true);
}

/* Dispatch DACP commands (dacp_play_pause, dacp_next, dacp_prev) or media controls with graceful fallback */
async function sendTransport(cmd) {
  if (isDemoAudioActive) {
    if (cmd === "playpause") toggleLocalPlayPause();
    else if (cmd === "nextitem") stepDemoTrack(1);
    else if (cmd === "previtem") stepDemoTrack(-1);
    return;
  }

  const dacpCmdMap = {
    playpause: "dacp_play_pause",
    nextitem: "dacp_next",
    previtem: "dacp_prev"
  };

  let dacpHandled = false;
  if (TAURI) {
    const directCmd = dacpCmdMap[cmd];
    try {
      let code;
      if (directCmd) {
        code = await invoke(directCmd).catch(() => invoke("dacp_command", { cmd }));
      } else {
        code = await invoke("dacp_command", { cmd });
      }
      if (typeof code === "number" && code >= 200 && code < 300) {
        dacpHandled = true;
      }
    } catch (_) {
      // quiet fallback when DACP endpoint not connected or returns error
    }
  }

  if (cmd === "playpause") {
    // Both on DACP success and as fallback when DACP is not active
    toggleLocalPlayPause();
  } else if (cmd === "nextitem") {
    if (npActive && !dacpHandled) {
      np.baseSec = 0;
      np.baseWall = Date.now();
      renderNpProgress();
    }
  } else if (cmd === "previtem") {
    if (npActive && !dacpHandled) {
      np.baseSec = 0;
      np.baseWall = Date.now();
      renderNpProgress();
    }
  }
}
["npPrev", "mPrev", "thPrev", "a2Prev"].forEach(id => {
  const el = $(id);
  if (el) el.addEventListener("click", () => sendTransport("previtem"));
});
["npNext", "mNext", "thNext", "a2Next"].forEach(id => {
  const el = $(id);
  if (el) el.addEventListener("click", () => sendTransport("nextitem"));
});
["npPlay", "mPlay", "thPlay", "a2Play"].forEach(id => {
  const el = $(id);
  if (el) el.addEventListener("click", () => sendTransport("playpause"));
});

function updateTransportControlsVisibility() {
  const show = !!runtimeSettings.showTransportControls;
  if ($("thTransport")) $("thTransport").hidden = !show;
  ["npPrev", "npPlay", "npNext"].forEach(id => {
    const el = $(id);
    if (el) el.hidden = !show;
  });
  ["mPrev", "mPlay", "mNext"].forEach(id => {
    const el = $(id);
    if (el) el.hidden = !show;
  });
  ["thPrev", "thPlay", "thNext"].forEach(id => {
    const el = $(id);
    if (el) el.hidden = !show;
  });
  ["a2Prev", "a2Play", "a2Next"].forEach(id => {
    const el = $(id);
    if (el) el.hidden = !show;
  });
}

let currentVol = 70;
let lastNonZeroVol = 70;
let volDebounce = null;

function updateVolUi(pct) {
  pct = Math.round(Number(pct) || 0);
  pct = Math.max(0, Math.min(100, pct));
  currentVol = pct;
  if (pct > 0) lastNonZeroVol = pct;

  if ($("npVol") && Number($("npVol").value) !== pct) $("npVol").value = pct;
  if ($("mVol") && Number($("mVol").value) !== pct) $("mVol").value = pct;
  if ($("a2Vol") && Number($("a2Vol").value) !== pct) $("a2Vol").value = pct;
  if ($("a2vol")) $("a2vol").textContent = pct + "%";

  const iconSvg = pct === 0
    ? '<path d="M4 9v6h4l5 5V4L8 9Z"/><line x1="17" y1="9" x2="23" y2="15"/><line x1="23" y1="9" x2="17" y2="15"/>'
    : (pct < 50
      ? '<path d="M4 9v6h4l5 5V4L8 9Z"/><path d="M16 9a5 5 0 0 1 0 6"/>'
      : '<path d="M4 9v6h4l5 5V4L8 9Z"/><path d="M16 9a5 5 0 0 1 0 6"/><path d="M19 6a9 9 0 0 1 0 12"/>');

  if ($("npVolIcon")) $("npVolIcon").innerHTML = iconSvg;
  if ($("mVolIcon")) $("mVolIcon").innerHTML = iconSvg;
  if ($("a2VolIcon")) $("a2VolIcon").innerHTML = iconSvg;
}

function sendVolume(pct) {
  updateVolUi(pct);
  if (!TAURI) return;
  clearTimeout(volDebounce);
  volDebounce = setTimeout(() => {
    invoke("audio_set_volume", { pct: Number(pct) }).catch(() => {});
    invoke("dacp_set_volume", { pct: Number(pct) }).catch(() => {});
  }, 40);
}

function toggleMute() {
  if (currentVol > 0) {
    lastNonZeroVol = currentVol;
    sendVolume(0);
  } else {
    sendVolume(lastNonZeroVol || 70);
  }
}

if ($("npVol")) $("npVol").addEventListener("input", (e) => sendVolume(e.target.value));
if ($("mVol")) $("mVol").addEventListener("input", (e) => sendVolume(e.target.value));
if ($("a2Vol")) $("a2Vol").addEventListener("input", (e) => sendVolume(e.target.value));
if ($("npVolBtn")) $("npVolBtn").addEventListener("click", toggleMute);
if ($("mVolBtn")) $("mVolBtn").addEventListener("click", toggleMute);
if ($("a2VolBtn")) $("a2VolBtn").addEventListener("click", toggleMute);

if (TAURI) {
  invoke("audio_get_volume").then((vol) => {
    if (typeof vol === "number" && !isNaN(vol)) {
      updateVolUi(vol);
    }
  }).catch(() => {});

  if (listen) {
    listen("audio://volume-changed", (e) => {
      if (e.payload && typeof e.payload.volume === "number" && !isNaN(e.payload.volume)) {
        updateVolUi(e.payload.volume);
      }
    });
  }
}

/* media keys & system MediaSession — trivial to wire, silent no-op when unsupported */
if ('mediaSession' in navigator) {
  try {
    navigator.mediaSession.setActionHandler('play', () => sendTransport("playpause"));
    navigator.mediaSession.setActionHandler('pause', () => sendTransport("playpause"));
    navigator.mediaSession.setActionHandler('previoustrack', () => sendTransport("previtem"));
    navigator.mediaSession.setActionHandler('nexttrack', () => sendTransport("nextitem"));
  } catch (_) {}
}
window.addEventListener("keydown", (e) => {
  if (e.key === "MediaPlayPause" || e.key === "MediaPlay" || e.key === "MediaPause") {
    sendTransport("playpause");
  } else if (e.key === "MediaTrackNext") {
    sendTransport("nextitem");
  } else if (e.key === "MediaTrackPrevious") {
    sendTransport("previtem");
  } else if ((e.key === " " || e.code === "Space") && !["INPUT", "TEXTAREA"].includes(document.activeElement?.tagName)) {
    if (npActive) {
      e.preventDefault();
      sendTransport("playpause");
    }
  }
});

/* enter/leave the live-mirror layout. `nextState` is the engine state to
   show after leaving (e.g. "stopped" when the engine died mid-mirror). */
function setMirroring(on, nextState, detail) {
  if (mirroring === on && !nextState) { /* idempotent */ }
  mirroring = on;
  document.body.classList.toggle("mirroring", on);
  $("theaterBtn").hidden = !on;
  $("zoomCtl").hidden = !on;
  updateIdleBg(); // single source of truth: hidden / unfocused / mirroring
  if (on) {
    applyEngineState("mirroring");
    if (runtimeSettings.autoTheater && !pipMode) {
      setTheater(true);
    }
    if (TAURI && !pipMode && runtimeSettings.alwaysOnTopMirror) {
      invoke("set_always_on_top", { on: true }).catch(() => {});
    }
  } else {
    setTheater(false);
    setZoom(100, true);
    if (TAURI && !pipMode && runtimeSettings.alwaysOnTopMirror) {
      invoke("set_always_on_top", { on: false }).catch(() => {});
    }
    applyEngineState(nextState || "ready", detail);
  }
  renderStage();
  reportRectSoon();
  refreshDevices();
}

/* --- rect reporting: keep the native video glued to #videoHost --- */
let lastRect = "";
function reportRect() {
  if (!TAURI || document.hidden) return;
  const dpr = window.devicePixelRatio || 1;
  let rect;
  if (document.body.classList.contains("theater")) {
    // theater: video takes the whole window, square corners, black behind
    rect = { x: 0, y: 0, w: Math.round(innerWidth * dpr), h: Math.round(innerHeight * dpr), radius: 0, zoom };
  } else {
    if (!$("receive").classList.contains("show")) return;
    const r = $("videoHost").getBoundingClientRect();
    if (r.width < 10 || r.height < 10) return;
    rect = {
      x: Math.round(r.left * dpr), y: Math.round(r.top * dpr),
      w: Math.round(r.width * dpr), h: Math.round(r.height * dpr),
      radius: Math.round(SCREEN_RADIUS * dpr), zoom
    };
  }
  const key = JSON.stringify(rect);
  if (key === lastRect) return;
  lastRect = key;
  invoke("set_video_rect", rect).catch(() => {});
}
function reportRectSoon() { requestAnimationFrame(() => requestAnimationFrame(reportRect)); }
if (TAURI) {
  // 4 Hz while a native video window is actually docked (it must stay glued to
  // the stage), 1 Hz otherwise — the idle ticks only exist to seed a target
  // rect before a session starts, and every tick is a forced layout read.
  let rectTick = 0;
  setInterval(() => { if (!document.hidden && (mirroring || rectTick++ % 4 === 0)) reportRect(); }, 250);
  window.addEventListener("resize", reportRectSoon);
  if (typeof ResizeObserver === "function") {
    new ResizeObserver(reportRectSoon).observe($("videoHost"));
  }
  document.querySelector(".stage").addEventListener("scroll", reportRect, { passive: true });
  reportRectSoon();
}

/* --- digital zoom --- */
function setZoom(z, silent) {
  zoom = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, z));
  const lbl = zoom + "%";
  $("zoomLbl").textContent = lbl;
  $("thZoomLbl").textContent = lbl;
  if (!silent) reportRect();
}
$("zoomIn").addEventListener("click", () => setZoom(zoom + ZOOM_STEP));
$("zoomOut").addEventListener("click", () => setZoom(zoom - ZOOM_STEP));
$("zoomLbl").addEventListener("click", () => setZoom(100));
$("thZoomIn").addEventListener("click", () => setZoom(zoom + ZOOM_STEP));
$("thZoomOut").addEventListener("click", () => setZoom(zoom - ZOOM_STEP));
$("thZoomLbl").addEventListener("click", () => setZoom(100));

/* --- theater mode + auto-hiding floating HUD --- */
let hudWakeTimer = null;
function wakeTheaterHud() {
  if (!document.body.classList.contains("theater")) return;
  document.body.classList.add("hud-wake");
  clearTimeout(hudWakeTimer);
  hudWakeTimer = setTimeout(() => {
    document.body.classList.remove("hud-wake");
  }, 2400);
}
function setTheater(on) {
  if (on && pipMode) setPipMode(false);
  document.body.classList.toggle("theater", !!on);
  if (on) wakeTheaterHud();
  else {
    clearTimeout(hudWakeTimer);
    document.body.classList.remove("hud-wake");
  }
  reportRectSoon();
}
$("theaterBtn").addEventListener("click", () => setTheater(!document.body.classList.contains("theater")));
$("thExit").addEventListener("click", () => setTheater(false));
window.addEventListener("pointermove", () => {
  if (document.body.classList.contains("theater")) wakeTheaterHud();
}, { passive: true });

/* --- compact PiP mini-player mode --- */
function setPipMode(on) {
  pipMode = !!on;
  if (pipMode && document.body.classList.contains("theater")) {
    setTheater(false);
  }
  if (pipMode) {
    nav(document.querySelector('[data-view="receive"]'));
    $("diagPopover").hidden = true;
    $("discoPill").setAttribute("aria-expanded", "false");
  }
  document.body.classList.toggle("pip", pipMode);
  $("pipBtn").classList.toggle("on", pipMode);
  updatePipTitle();
  if (TAURI) {
    invoke("set_pip_mode", {
      pip: pipMode,
      restoreAlwaysOnTop: !!(mirroring && runtimeSettings.alwaysOnTopMirror)
    }).catch(() => {});
  }
  reportRectSoon();
  setTimeout(updatePipTextScroll, 50);
  setTimeout(updatePipTextScroll, 220);
}
$("pipBtn").addEventListener("click", () => setPipMode(!pipMode));
$("pipExitBtn").addEventListener("click", () => setPipMode(false));

if (window.ResizeObserver) {
  const pipResizeObs = new ResizeObserver(() => {
    if (pipMode) updatePipTextScroll();
  });
  if ($("npMetaWrap")) pipResizeObs.observe($("npMetaWrap"));
  pipResizeObs.observe(document.body);
} else {
  window.addEventListener("resize", () => {
    if (pipMode) updatePipTextScroll();
  }, { passive: true });
}

/* --- slide-over spatial ecosystem drawer (Overhaul A) --- */
let drawerOpen = false;
let airplayDeviceCount = 0;

function updateEcosystemBadge() {
  const badge = $("drawerCountBadge");
  if (!badge) return;
  badge.textContent = String(airplayDeviceCount + peerCache.length);
}

function setDrawerOpen(open) {
  drawerOpen = !!open;
  const dr = $("spatialDrawer") || $("ecoDrawer");
  const btn = $("drawerToggleBtn");
  if (dr) dr.classList.toggle("closed", !drawerOpen);
  if (btn) {
    btn.classList.toggle("active", drawerOpen);
    btn.setAttribute("aria-expanded", String(drawerOpen));
  }
  document.body.classList.toggle("drawer-open", drawerOpen);
  reportRectSoon();
}
$("drawerToggleBtn").addEventListener("click", () => setDrawerOpen(!drawerOpen));
$("drawerCloseBtn").addEventListener("click", () => setDrawerOpen(false));
$("drawerOpenFolder").addEventListener("click", () => revealPath(null));
$("drawerGoLocalSend").addEventListener("click", () => {
  setDrawerOpen(false);
  nav(document.querySelector('[data-view="localsend"]'));
});
$("idleDropChip").addEventListener("click", () => {
  nav(document.querySelector('[data-view="localsend"]'));
});

/* --- network & engine diagnostics popover --- */
function updateSpatialMetaBadge(ipStr) {
  const b = $("spatialMetaBadge");
  if (!b) return;
  const fpsTag = `${runtimeSettings.fps || 60}fps`;
  const cleanIp = ipStr && ipStr !== "—" && ipStr !== "Unavailable" && ipStr !== "Detecting…" ? ipStr : "D3D11";
  b.textContent = `${cleanIp} · ${fpsTag}`;
}

async function refreshDiagnostics() {
  if (!TAURI) {
    $("diagName").textContent = currentBroadcastName;
    $("diagIp").textContent = "192.168.1.42";
    const mode = runtimeSettings.fps === 30 ? "30fps Eco" : (runtimeSettings.vsync ? "60fps AV-Sync" : "60fps Low-Lat");
    $("diagCodec").textContent = `H.264 D3D11 · ${mode}`;
    $("diagLsPort").textContent = "53317 · HTTP/UDP";
    updateSpatialMetaBadge("192.168.1.42");
    return;
  }
  try {
    const d = await invoke("network_diagnostics");
    if (!d) return;
    $("diagName").textContent = d.broadcastName || currentBroadcastName;
    $("diagIp").textContent = d.lanIp || "Unavailable";
    const mode = d.fps === 30 ? "30fps Eco" : (d.vsync ? "60fps AV-Sync" : "60fps Low-Lat");
    $("diagCodec").textContent = `${d.hwDecode || "H.264 D3D11"} · ${mode}`;
    $("diagLsPort").textContent = `${d.localsendPort || 53317} · HTTP/UDP`;
    updateSpatialMetaBadge(d.lanIp);
  } catch (_) {}
}
$("discoPill").addEventListener("click", (e) => {
  e.stopPropagation();
  const nextHidden = !$("diagPopover").hidden;
  $("diagPopover").hidden = nextHidden;
  $("discoPill").setAttribute("aria-expanded", String(!nextHidden));
  if (!nextHidden) refreshDiagnostics();
});
document.addEventListener("click", (e) => {
  const pop = $("diagPopover");
  if (!pop || pop.hidden) return;
  if (!pop.contains(e.target) && !$("discoPill").contains(e.target)) {
    pop.hidden = true;
    $("discoPill").setAttribute("aria-expanded", "false");
  }
});
$("diagCopyIp").addEventListener("click", () => {
  const ip = $("diagIp").textContent || "";
  if (navigator.clipboard && ip && ip !== "—" && ip !== "Unavailable") {
    navigator.clipboard.writeText(ip).then(() => toast(`Copied ${ip}`));
  } else {
    toast("No LAN IP available to copy", true);
  }
});
$("diagRestart").addEventListener("click", async () => {
  $("diagPopover").hidden = true;
  $("discoPill").setAttribute("aria-expanded", "false");
  if (!TAURI) {
    toast("Receiver restarted (demo)");
    return;
  }
  applyEngineState("starting");
  try {
    await invoke("stop_airplay_receiver").catch(() => {});
    await invoke("start_airplay_receiver");
    toast("AirPlay receiver restarted");
    refreshDiagnostics();
  } catch (e) {
    applyEngineState("error", String(e));
    toast(String(e), true);
  }
});

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!$("diagPopover").hidden) {
      $("diagPopover").hidden = true;
      $("discoPill").setAttribute("aria-expanded", "false");
      return;
    }
    if (drawerOpen) {
      setDrawerOpen(false);
      return;
    }
    if (document.body.classList.contains("theater")) {
      setTheater(false);
      return;
    }
    if (pipMode) {
      setPipMode(false);
      return;
    }
  }
  if (!mirroring) return;
  if (e.key === "+" || e.key === "=") setZoom(zoom + ZOOM_STEP);
  else if (e.key === "-" || e.key === "_") setZoom(zoom - ZOOM_STEP);
  else if (e.key === "0") setZoom(100);
});

$("rxRetry").addEventListener("click", () => {
  applyEngineState("starting");
  invoke && invoke("start_airplay_receiver").catch(e => applyEngineState("error", String(e)));
});

/* receiver toggle (Tauri) / demo simulate (browser) */
const rxBtn = $("rxBtn");
if (TAURI) {
  rxBtn.classList.remove("primary");
  rxBtn.classList.add("ghost");
  rxBtn.textContent = "Stop receiver";
  rxBtn.addEventListener("click", () => {
    if (engineState === "stopped" || engineState === "error") {
      applyEngineState("starting");
      invoke("start_airplay_receiver").catch(e => applyEngineState("error", String(e)));
    } else {
      invoke("stop_airplay_receiver").catch(() => {});
    }
  });
} else {
  rxBtn.addEventListener("click", () => {
    const on = !$("rxMirror").classList.contains("live");
    if (on) stopDemoNowPlaying(); // mirror wins — leave the Now Playing demo
    $("rxMirror").classList.toggle("live", on);
    setMirroring(on);
    if (on) {
      $("rxState").textContent = "Connected · AirPlay Mirror · 1080p60 · ~42 ms latency";
    } else {
      $("rxState").textContent = "Idle — demo";
    }
    rxBtn.textContent = on ? "Disconnect" : "Simulate device connect";
    rxBtn.classList.toggle("danger", on);
    rxBtn.classList.toggle("primary", !on);
  });
  // demo mode: explore the Now Playing stage without a real device,
  // reusing the same track info as the AirPlay-2 "Simulate stream" demo
  $("npDemoBtn").hidden = false;
  $("npDemoBtn").addEventListener("click", () => {
    if (npActive) stopDemoNowPlaying(); else startDemoNowPlaying();
  });
}

function startDemoNowPlaying() {
  if ($("rxMirror").classList.contains("live")) return; // mirror wins
  isDemoAudioActive = true;
  npActive = true;
  dacpAvailable = true;
  const t = DEMO_PLAYLIST[demoTrackIdx];
  setNpMeta({ title: t.title, artist: t.artist, album: t.album, hasData: true, rev: Date.now() });
  setNpArt(t.art || null);
  setNpProgress(12, t.duration, 0);
  updatePlayIcon(true);
  $("npDemoBtn").textContent = "Stop audio stream";
  renderStage();
}
function stopDemoNowPlaying() {
  if (!npActive) return;
  isDemoAudioActive = false;
  npActive = false;
  dacpAvailable = false;
  resetNpProgress();
  setNpArt(null);
  $("npDemoBtn").textContent = "Simulate audio stream";
  renderStage();
  $("rxState").textContent = "Idle — demo"; // same override loadDemo() uses
}

let lastDevicesJson = "";
let refreshDevices = async () => {};

function pickDeviceIcon(d) {
  if (!d) return ICON.generic;
  const str = `${d.type || ""} ${d.kind || ""} ${d.name || ""} ${d.meta || ""}`.toLowerCase();
  if (str.includes("iphone") || str.includes("ios") || str.includes("apple device")) return ICON.iphone;
  if (str.includes("ipad")) return ICON.ipad;
  if (str.includes("mac")) return ICON.mac;
  if (str.includes("android") || str.includes("pixel") || str.includes("galaxy")) return ICON.android;
  if (str.includes("speaker") || str.includes("homepod") || str.includes("express") || str.includes("audio")) return ICON.speaker;
  return ICON.generic;
}

function renderAirplayDevices(list) {
  if (!Array.isArray(list)) list = [];
  airplayDeviceCount = list.filter(d => d.online !== false).length;
  updateEcosystemBadge();
  const json = JSON.stringify(list);
  const targets = [
    $("airplayDevices"),
    $("rxDevices"),
    $("rxDevicesMain"),
    $("ecoPeers"),
    document.querySelector('[data-alias="airplayDevices"]'),
    document.querySelector('[data-alias="ecoPeers"]')
  ].filter((el, idx, arr) => el && arr.indexOf(el) === idx);

  const hasEmptyTarget = targets.some(t => !t.children.length);
  if (json === lastDevicesJson && !hasEmptyTarget) return;
  lastDevicesJson = json;

  targets.forEach(g => {
    g.innerHTML = "";
    if (!list.length) {
      g.innerHTML = `<div class="card tile"><div class="ic">${ICON.generic}</div><div style="flex:1"><div class="name">Nothing seen yet</div>
        <div class="meta">Apple devices on this network appear here</div></div></div>`;
      return;
    }
    list.forEach(d => {
      const el = document.createElement("div"); el.className = "card tile";
      el.innerHTML = `<div class="ic">${pickDeviceIcon(d)}</div>
        <div style="flex:1;min-width:0"><div class="name">${esc(d.name)}</div><div class="meta">${esc(d.meta || "")}</div></div>
        <div class="status"><span class="dot ${d.online ? "" : "off"}"></span>${d.online ? "Online" : "Offline"}</div>`;
      g.appendChild(el);
    });
  });
}

/* ============================================================
   equalizer bars
   ============================================================ */
function buildWave(id) {
  const w = $(id);
  if (!w) return;
  for (let i = 0; i < 22; i++) {
    const b = document.createElement("i");
    b.style.animationDelay = (i * 0.06) + "s";
    b.style.animationDuration = (0.7 + ((i * 7919) % 100) / 140) + "s";
    w.appendChild(b);
  }
}
buildWave("a2wave"); buildWave("castwave");

/* ---------- AirPlay 2 ---------- */
let a2Running = true;
let a2ActiveSession = false;

function setA2RunningState(running) {
  a2Running = running;
  const btn = $("a2btn");
  if (btn) {
    btn.textContent = running ? "Stop AirPlay 2" : "Start AirPlay 2";
    btn.className = running ? "btn" : "btn primary";
  }
  const st = $("a2status");
  if (st) {
    if (!running) {
      st.textContent = "Engine Stopped";
      st.className = "tag";
    } else if (a2ActiveSession) {
      st.textContent = "Streaming Lossless";
      st.className = "tag good";
    } else {
      st.textContent = "Ready · " + (currentBroadcastName || "Mirr") + " Audio";
      st.className = "tag good";
    }
  }
  const mdnsTag = $("a2mdnsName");
  if (mdnsTag) {
    mdnsTag.textContent = (currentBroadcastName || "Mirr") + " Audio";
  }
}

if ($("a2btn")) {
  $("a2btn").addEventListener("click", function () {
    if (TAURI) {
      if (a2Running) {
        invoke("stop_airplay2_receiver").then(
          () => { setA2RunningState(false); toast("AirPlay 2 receiver stopped"); },
          (e) => toast(String(e), true)
        );
      } else {
        invoke("start_airplay2_receiver").then(
          () => { setA2RunningState(true); toast("AirPlay 2 receiver started"); },
          (e) => toast(String(e), true)
        );
      }
      return;
    }
    const t = $("a2track"), s = $("a2sub"), w = $("a2wave");
    if (t.textContent === "Nothing playing") {
      t.textContent = DEMO_TRACK.title;
      s.textContent = "From iPhone · Lossless 44.1 kHz PCM";
      if (w) w.classList.add("on"); this.textContent = "Stop";
    } else {
      t.textContent = "Nothing playing";
      s.textContent = "Waiting for an AirPlay 2 source · 44.1 kHz 16-bit PCM";
      if (w) w.classList.remove("on"); this.textContent = "Simulate stream";
    }
  });
}

const roomCache = {};
function renderRooms(targetId, list, onToggle) {
  const rc = $(targetId);
  if (!rc) return;
  const json = JSON.stringify(list);
  if (roomCache[targetId] === json) return;
  roomCache[targetId] = json;
  rc.innerHTML = "";
  (list || []).forEach(r => {
    const el = document.createElement("div"); el.className = "room tile";
    el.innerHTML = `<div class="ricon">${ICON.speaker}</div>
      <div class="st" style="flex:1"><div class="rname">${esc(r.name)}</div><div class="rmeta">${esc(r.meta || "")}</div></div>
      <div class="slider" style="max-width:120px"><input type="range" min="0" max="100" value="${Number(r.vol ?? 60)}" aria-label="Speaker volume"></div>
      <div class="switch ${r.on ? "on" : ""}" role="switch" aria-checked="${r.on ? "true" : "false"}"></div>`;
    const sw = el.querySelector(".switch");
    sw.addEventListener("click", () => {
      const nextOn = !sw.classList.contains("on");
      sw.classList.toggle("on", nextOn);
      sw.setAttribute("aria-checked", String(nextOn));
      r.on = nextOn;
      if (onToggle) onToggle(r, sw, nextOn);
    });
    rc.appendChild(el);
  });
}

/* ---------- AirCast ---------- */
function syncCastWave() {
  const anyOn = !!$("rooms").querySelector(".switch.on") || $("castPlay").textContent === "⏸ Streaming";
  $("castwave").classList.toggle("on", anyOn);
  $("castPlay").textContent = anyOn ? "⏸ Streaming" : "▶ Stream";
}
$("castPlay").addEventListener("click", function () {
  if (TAURI) {
    const activeSw = $("rooms").querySelector(".switch.on");
    if (activeSw) {
      activeSw.click();
    } else {
      toast("Pick a speaker below to start casting");
    }
    return;
  }
  const streaming = this.textContent !== "⏸ Streaming";
  this.textContent = streaming ? "⏸ Streaming" : "▶ Stream";
  $("castwave").classList.toggle("on", streaming);
});
function castTo(target, sw, nextOn = true) {
  if (!TAURI) { syncCastWave(); return; }
  if (!nextOn) {
    if (!$("rooms").querySelector(".switch.on")) {
      invoke("stop_aircast").catch(() => {});
    }
    syncCastWave();
    return;
  }
  // single-target sidecar: uncheck any previously active speaker switch
  $("rooms").querySelectorAll(".switch.on").forEach(other => {
    if (other !== sw) {
      other.classList.remove("on");
      other.setAttribute("aria-checked", "false");
    }
  });
  syncCastWave();
  invoke("start_aircast", { targetIp: target.addr || "" }).then(
    () => toast(`Casting to ${target.name}`),
    (e) => {
      if (sw) {
        sw.classList.remove("on");
        sw.setAttribute("aria-checked", "false");
      }
      target.on = false;
      syncCastWave();
      toast(String(e), true);
    }
  );
}

/* ============================================================
   LocalSend
   ============================================================ */
let peerCache = [];
let selectedPeer = null;
let lastPeersJson = "";

function selectPeer(peer) {
  selectedPeer = peer || null;
  ["lsPeers", "drawerPeers", "discoPeers"].forEach(id => {
    const pl = $(id);
    if (pl) {
      pl.querySelectorAll(".peer-row").forEach(r => {
        r.classList.toggle("sel", !!(selectedPeer && r.dataset.fp === selectedPeer.fingerprint));
      });
    }
  });
  $("dropHint").textContent = selectedPeer ? `sends to ${selectedPeer.alias}` : "or pick a device from the list";
}

function populatePeerContainer(container, list) {
  if (!container) return;
  container.innerHTML = "";
  if (!list.length) {
    container.innerHTML = `<div class="peer-row tile"><div class="pic">${ICON.generic}</div><div><div class="pn">No devices yet</div><div class="pm">Open LocalSend on another device on this network</div></div></div>`;
    return;
  }
  list.forEach(p => {
    const el = document.createElement("div"); el.className = "peer-row tile";
    el.dataset.fp = p.fingerprint || "";
    if (selectedPeer && selectedPeer.fingerprint === p.fingerprint) el.classList.add("sel");
    el.innerHTML = `<div class="pic">${pickIcon(p.type || p.model || p.alias)}</div>
      <div style="flex:1;min-width:0"><div class="pn">${esc(p.alias)}</div><div class="pm">${esc(p.model || "")}</div></div>
      <button class="btn sm" type="button">Send</button>`;
    el.querySelector("button").addEventListener("click", e => { e.stopPropagation(); pickAndSend(p); });
    el.addEventListener("click", () => selectPeer(p));
    container.appendChild(el);
  });
}

function renderPeers(list) {
  const json = JSON.stringify(list);
  peerCache = list;
  updateEcosystemBadge();
  if (selectedPeer && !list.some(p => p.fingerprint === selectedPeer.fingerprint)) {
    selectPeer(null);
  }
  if (json === lastPeersJson) return;
  lastPeersJson = json;
  populatePeerContainer($("lsPeers"), list);
  populatePeerContainer($("drawerPeers"), list);
  const disco = $("discoPeers");
  if (disco && disco !== $("drawerPeers") && disco !== $("lsPeers")) {
    populatePeerContainer(disco, list);
  }
  $("lsScan").textContent = list.length ? `· ${list.length} nearby` : "· scanning…";
}

function showTransfer(name, dev, busy) {
  $("transfer").classList.add("show");
  $("tfName").textContent = name;
  $("tfDev").textContent = dev;
  $("tfBarWrap").classList.toggle("busy", !!busy);
  $("tfBar").style.width = busy ? "40%" : "0%";
  $("tfBar").style.background = "";
  $("tfPct").textContent = busy ? "Sending…" : "";
}
function finishTransfer(ok, msg) {
  $("tfBarWrap").classList.remove("busy");
  $("tfBar").style.width = "100%";
  $("tfBar").style.background = ok ? "" : "rgba(255,120,120,.8)";
  $("tfPct").textContent = ok ? "✓ Delivered" : ("✕ " + msg);
}

const basename = (p) => String(p).split(/[\\/]/).pop();

/* --- LocalSend recent transfer history --- */
const transferHistory = [];
function fmtClock(d) {
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
function revealPath(path) {
  if (!TAURI) {
    toast(path ? `Would reveal ${basename(path)} in Explorer` : "Would open Downloads folder in Explorer");
    return;
  }
  invoke("reveal_in_explorer", { path: path || null }).catch((e) => toast(String(e), true));
}
$("lsOpenFolder").addEventListener("click", () => revealPath(null));

function recordTransferHistory({ dir, name, peer, path }) {
  transferHistory.unshift({
    dir: dir === "in" ? "in" : "out",
    name: name || (path ? basename(path) : "file"),
    peer: peer || "Device",
    path: path || null,
    time: fmtClock(new Date())
  });
  if (transferHistory.length > 8) transferHistory.length = 8;
  renderTransferHistory();
}

function renderTransferHistory() {
  const wrap = $("lsHistoryWrap"), list = $("lsHistory");
  if (!wrap || !list) return;
  if (!transferHistory.length) {
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  list.innerHTML = "";
  transferHistory.forEach((item) => {
    const row = document.createElement("div");
    row.className = "hist-row";
    const arrow = item.dir === "in" ? "↓" : "↑";
    const dirLabel = item.dir === "in" ? `From ${item.peer}` : `To ${item.peer}`;
    row.innerHTML = `<div class="hist-dir ${item.dir}" title="${item.dir === "in" ? "Received" : "Sent"}">${arrow}</div>
      <div class="hist-info">
        <div class="hist-name">${esc(item.name)}</div>
        <div class="hist-meta">${esc(dirLabel)} · ${esc(item.time)}</div>
      </div>
      <button class="btn ghost sm" type="button">${item.path ? "Reveal" : "Folder"}</button>`;
    row.querySelector("button").addEventListener("click", () => revealPath(item.path));
    list.appendChild(row);
  });
}

async function sendPath(peer, path) {
  const name = basename(path);
  showTransfer(name, peer.alias, true);
  try {
    await invoke("localsend_send", { targetIp: peer.ip, port: peer.port, path });
    finishTransfer(true);
    recordTransferHistory({ dir: "out", name, peer: peer.alias, path });
    return true;
  } catch (e) {
    finishTransfer(false, String(e));
    return false;
  }
}

async function pickAndSend(peer) {
  selectPeer(peer);
  if (!TAURI) { fakeTransfer(peer.alias); return; }
  const file = await window.__TAURI__.dialog.open({ multiple: false, title: "Send a file with LocalSend" }).catch(() => null);
  if (!file) return;
  sendPath(peer, typeof file === "string" ? file : file.path || String(file));
}

function fakeTransfer(dev) {
  const names = ["photo.heic", "presentation.pdf", "clip.mov", "design.fig", "notes.md"];
  const picked = names[Math.floor(Math.random() * names.length)];
  showTransfer(picked, dev, false);
  const bar = $("tfBar"), pct = $("tfPct");
  let p = 0;
  const iv = setInterval(() => {
    p += Math.random() * 9 + 3;
    if (p >= 100) {
      p = 100;
      clearInterval(iv);
      pct.textContent = "✓ Delivered";
      recordTransferHistory({ dir: "out", name: picked, peer: dev, path: null });
    }
    bar.style.width = p + "%";
    if (p < 100) pct.textContent = "Sending… " + Math.floor(p) + "%";
  }, 160);
}

/* drop zone + whole-window drag-and-drop overlay */
const drop = $("drop");
const dropOverlay = $("dropOverlay");
function updateDropOverlayHint() {
  const peer = selectedPeer || (peerCache.length === 1 ? peerCache[0] : null);
  $("dropOverlaySub").textContent = peer
    ? `Release to send directly to ${peer.alias}`
    : "Release to switch to LocalSend and send to your selected device";
}
function showDropOverlay(on) {
  if (!dropOverlay) return;
  if (on) updateDropOverlayHint();
  dropOverlay.classList.toggle("show", !!on);
}

let browserDragDepth = 0;
window.addEventListener("dragenter", (e) => {
  if (TAURI) return;
  e.preventDefault();
  browserDragDepth++;
  showDropOverlay(true);
});
window.addEventListener("dragover", (e) => {
  e.preventDefault();
});
window.addEventListener("dragleave", () => {
  if (TAURI) return;
  browserDragDepth = Math.max(0, browserDragDepth - 1);
  if (browserDragDepth === 0) showDropOverlay(false);
});
window.addEventListener("drop", (e) => {
  if (TAURI) return;
  e.preventDefault();
  browserDragDepth = 0;
  showDropOverlay(false);
  drop.classList.remove("hot");
  nav(document.querySelector('[data-view="localsend"]'));
  const peer = selectedPeer || (peerCache.length === 1 ? peerCache[0] : null);
  fakeTransfer((peer && peer.alias) || "LocalSend Device");
});

drop.addEventListener("dragover", e => { e.preventDefault(); drop.classList.add("hot"); });
drop.addEventListener("dragleave", () => drop.classList.remove("hot"));
drop.addEventListener("drop", e => {
  e.preventDefault();
  drop.classList.remove("hot");
  showDropOverlay(false);
});
if (listen) {
  listen("tauri://drag-enter", () => {
    drop.classList.add("hot");
    showDropOverlay(true);
  });
  listen("tauri://drag-leave", () => {
    drop.classList.remove("hot");
    showDropOverlay(false);
  });
  listen("tauri://drag-drop", async (e) => {
    drop.classList.remove("hot");
    showDropOverlay(false);
    const paths = (e.payload && e.payload.paths) || [];
    if (!paths.length) return;
    if (!$("localsend").classList.contains("show")) {
      nav(document.querySelector('[data-view="localsend"]'));
    }
    const peer = selectedPeer || (peerCache.length === 1 ? peerCache[0] : null);
    if (!peer) { toast("Pick a device first — click one in the list", true); return; }
    for (const p of paths) {
      if (!await sendPath(peer, p)) break;
    }
  });
}

/* ============================================================
   Interactive Runtime Settings
   ============================================================ */
let availableAudioDevices = [];

async function refreshAudioDevicesList() {
  if (!TAURI) {
    availableAudioDevices = [
      { id: "default", name: "System Default", isDefault: true },
      { id: "mock-1", name: "Headphones (AirPods Pro)", isDefault: true },
      { id: "mock-2", name: "Speakers (Realtek Audio)", isDefault: false }
    ];
    renderAudioDeviceOptions();
    return;
  }
  try {
    const list = await invoke("list_audio_devices").catch(() => []);
    availableAudioDevices = list || [];
    renderAudioDeviceOptions();
  } catch (e) {
    console.warn("Failed to list audio devices:", e);
  }
}

function renderAudioDeviceOptions() {
  const sel = $("setAudioOutputSelect");
  if (!sel) return;
  const currentVal = runtimeSettings.audioOutputDevice || "default";
  sel.innerHTML = "";

  const defDev = availableAudioDevices.find(d => (d.is_default ?? d.isDefault));
  let defTag = "";
  if (defDev) {
    const isAtmos = !!(defDev.is_dolby_atmos ?? defDev.isDolbyAtmos);
    const isSpatial = !!(defDev.supports_spatial ?? defDev.supportsSpatial);
    if (isAtmos) defTag = " · Dolby Atmos";
    else if (isSpatial) defTag = " · Spatial Audio";
  }
  const defLabel = defDev ? `Windows Default (${defDev.name}${defTag})` : "Windows Default";
  const defOpt = document.createElement("option");
  defOpt.value = "default";
  defOpt.textContent = defLabel;
  sel.appendChild(defOpt);

  availableAudioDevices.forEach((d) => {
    const opt = document.createElement("option");
    opt.value = d.id;
    let label = d.name;
    const isAtmos = !!(d.is_dolby_atmos ?? d.isDolbyAtmos);
    const isSpatial = !!(d.supports_spatial ?? d.supportsSpatial);
    const isDef = !!(d.is_default ?? d.isDefault);
    if (isAtmos) {
      label += " [Dolby Atmos]";
    } else if (isSpatial) {
      label += " [Spatial Audio]";
    }
    if (isDef) {
      label += " (Default)";
    }
    opt.textContent = label;
    sel.appendChild(opt);
  });

  sel.value = currentVal;
  if (!sel.value) sel.value = "default";
}

let currentSpatialCaps = {
  isSpatialSupported: false,
  isDolbyAtmos: false,
  spatialTechnology: "Stereo",
  maxChannels: 2
};

function updateSpatialAudioUI(caps) {
  if (!caps) return;
  const isAtmos = !!(caps.is_dolby_atmos ?? caps.isDolbyAtmos ?? caps.dolbyAtmos);
  const isSpatial = !!(caps.is_spatial_supported ?? caps.isSpatialSupported ?? caps.spatialAudio ?? isAtmos);
  const tech = caps.spatial_technology || caps.spatialTechnology || (isAtmos ? "Dolby Atmos (Hardware / DAX3)" : (isSpatial ? "Windows Spatial Audio" : "Stereo"));
  const maxChannels = caps.max_channels || caps.maxChannels || (isAtmos ? 8 : 2);

  currentSpatialCaps = {
    isSpatialSupported: isSpatial,
    isDolbyAtmos: isAtmos,
    spatialTechnology: tech,
    maxChannels
  };

  const a2Badge = $("a2spatialBadge");
  const a2Tech = $("a2spatialTech");
  const a2Sub = $("a2spatialSub");
  const a2AtmosBadge = $("a2atmosBadge");
  const npAtmosBadge = $("npAtmosBadge");
  const a2MdnsModel = $("a2mdnsModel");
  const a2Format = $("a2audioFormat");
  const a2FormatSub = $("a2audioFormatSub");

  if (a2Badge) {
    a2Badge.style.display = isAtmos ? "inline-block" : "none";
  }
  if (a2AtmosBadge) {
    a2AtmosBadge.style.display = isAtmos ? "inline-block" : "none";
  }
  if (npAtmosBadge) {
    npAtmosBadge.style.display = isAtmos ? "inline-block" : "none";
  }

  if (a2Tech) {
    if (isAtmos) {
      a2Tech.textContent = tech || "Dolby Atmos Active";
    } else if (isSpatial) {
      a2Tech.textContent = tech || "Windows Spatial Audio";
    } else {
      a2Tech.textContent = "Standard 2-Channel Stereo";
    }
  }

  if (a2Sub) {
    if (isAtmos) {
      a2Sub.textContent = "HomePod 2nd Gen · Dolby Atmos Spatial Profile Advertised";
    } else if (isSpatial) {
      a2Sub.textContent = "Spatial Audio Bed Matrix · Multi-Channel Output";
    } else {
      a2Sub.textContent = "Direct WASAPI 2-Channel Audio Engine";
    }
  }

  if (a2MdnsModel) {
    if (isAtmos) {
      a2MdnsModel.textContent = "Apple HomePod (2nd gen) · Spatial Audio Model";
    } else {
      a2MdnsModel.textContent = "OpenAirPlay2 Speaker · Port 7000";
    }
  }

  if (a2Format) {
    if (isAtmos) {
      a2Format.textContent = "48.0 / 44.1 kHz · Dolby Atmos Spatial Bed";
    } else {
      a2Format.textContent = "44.1 kHz · 16-bit Lossless Stereo";
    }
  }
  if (a2FormatSub) {
    if (isAtmos) {
      a2FormatSub.textContent = "Direct WASAPI Spatial Matrix Output";
    } else {
      a2FormatSub.textContent = "Direct WASAPI Low-Latency Output";
    }
  }
}

function syncSettingsUI() {
  const inp = $("setBroadcastInput");
  if (inp) inp.value = runtimeSettings.broadcastName || currentBroadcastName;
  $("setName").textContent = runtimeSettings.broadcastName || currentBroadcastName;
  const presetKey = runtimeSettings.fps === 30 ? "30-eco" : (runtimeSettings.vsync ? "60-sync" : "60-low");
  document.querySelectorAll("#setPresetGroup .seg-btn").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.preset === presetKey);
  });
  const recMode = runtimeSettings.receiverMode || "dual";
  document.querySelectorAll("#setReceiverModeGroup .seg-btn").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.mode === recMode);
  });
  const aot = $("setAlwaysOnTop");
  if (aot) {
    aot.classList.toggle("on", !!runtimeSettings.alwaysOnTopMirror);
    aot.setAttribute("aria-checked", String(!!runtimeSettings.alwaysOnTopMirror));
  }
  const ath = $("setAutoTheater");
  if (ath) {
    ath.classList.toggle("on", !!runtimeSettings.autoTheater);
    ath.setAttribute("aria-checked", String(!!runtimeSettings.autoTheater));
  }
  const asw = $("setAutoSwitchAudio");
  if (asw) {
    const on = runtimeSettings.autoSwitchAudioOutput !== false;
    asw.classList.toggle("on", on);
    asw.setAttribute("aria-checked", String(on));
  }
  const pda = $("setPreferDolbyAtmos");
  if (pda) {
    const on = runtimeSettings.preferDolbyAtmos !== false;
    pda.classList.toggle("on", on);
    pda.setAttribute("aria-checked", String(on));
  }
  const audSel = $("setAudioOutputSelect");
  if (audSel) {
    audSel.value = runtimeSettings.audioOutputDevice || "default";
  }
  const stc = $("setShowTransportControls");
  if (stc) {
    const on = !!runtimeSettings.showTransportControls;
    stc.classList.toggle("on", on);
    stc.setAttribute("aria-checked", String(on));
  }
  updateTransportControlsVisibility();
  applyTheme(runtimeSettings.bgTheme || "horizon");
  refreshDiagnostics();
}

async function persistSettings(restartReceiver = false) {
  currentBroadcastName = runtimeSettings.broadcastName || "Mirr";
  syncSettingsUI();
  if ($("receive").classList.contains("show")) {
    $("vsub").textContent = `Your PC appears as “${currentBroadcastName}”`;
  }
  if (!mirroring && !npActive && engineState === "ready") {
    applyEngineState("ready");
  } else {
    updatePipTitle();
  }
  try {
    localStorage.setItem("mirr_settings", JSON.stringify(runtimeSettings));
  } catch (_) {}

  if (!TAURI) {
    toast(restartReceiver ? `Saved & updated broadcast as “${currentBroadcastName}”` : "Preferences updated");
    return;
  }
  try {
    await invoke("update_settings", {
      config: {
        broadcastName: runtimeSettings.broadcastName,
        fps: runtimeSettings.fps,
        vsync: runtimeSettings.vsync,
        alwaysOnTopMirror: runtimeSettings.alwaysOnTopMirror,
        autoTheater: runtimeSettings.autoTheater,
        audioOutputDevice: runtimeSettings.audioOutputDevice || "default",
        autoSwitchAudioOutput: runtimeSettings.autoSwitchAudioOutput !== false,
        preferDolbyAtmos: runtimeSettings.preferDolbyAtmos !== false,
        showTransportControls: !!runtimeSettings.showTransportControls,
        receiverMode: runtimeSettings.receiverMode || "dual"
      },
      broadcastName: runtimeSettings.broadcastName,
      fps: runtimeSettings.fps,
      vsync: runtimeSettings.vsync,
      alwaysOnTopMirror: runtimeSettings.alwaysOnTopMirror,
      autoTheater: runtimeSettings.autoTheater,
      audioOutputDevice: runtimeSettings.audioOutputDevice || "default",
      autoSwitchAudioOutput: runtimeSettings.autoSwitchAudioOutput !== false,
      preferDolbyAtmos: runtimeSettings.preferDolbyAtmos !== false,
      showTransportControls: !!runtimeSettings.showTransportControls,
      receiverMode: runtimeSettings.receiverMode || "dual",
      restartReceiver: !!restartReceiver
    });
    if (mirroring && !pipMode) {
      invoke("set_always_on_top", { on: !!runtimeSettings.alwaysOnTopMirror }).catch(() => {});
    }
    if (restartReceiver && engineState !== "stopped") {
      applyEngineState("starting");
      await invoke("stop_airplay_receiver").catch(() => {});
      await invoke("start_airplay_receiver");
      toast(`Receiver restarted (${runtimeSettings.fps} fps)`);
    } else {
      toast("Settings saved");
    }
  } catch (e) {
    toast(String(e), true);
  }
}

document.querySelectorAll("#themeGrid .theme-card").forEach((card) => {
  const pick = () => {
    const t = card.dataset.theme;
    if (t) {
      applyTheme(t);
      persistSettings(false);
      const nameEl = card.querySelector(".theme-card-name");
      toast(`Theme set to ${nameEl ? nameEl.textContent : t}`);
    }
  };
  card.addEventListener("click", pick);
  card.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      pick();
    }
  });
});

$("setSaveName").addEventListener("click", () => {
  const raw = ($("setBroadcastInput").value || "").trim();
  if (!raw) {
    toast("Broadcast name cannot be empty", true);
    return;
  }
  runtimeSettings.broadcastName = raw;
  persistSettings(true);
});
$("setBroadcastInput").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("setSaveName").click();
});
document.querySelectorAll("#setPresetGroup .seg-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    const p = btn.dataset.preset;
    if (p === "30-eco") { runtimeSettings.fps = 30; runtimeSettings.vsync = false; }
    else if (p === "60-sync") { runtimeSettings.fps = 60; runtimeSettings.vsync = true; }
    else { runtimeSettings.fps = 60; runtimeSettings.vsync = false; }
    persistSettings(true);
  });
});
document.querySelectorAll("#setReceiverModeGroup .seg-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    const m = btn.dataset.mode;
    if (m) {
      runtimeSettings.receiverMode = m;
      persistSettings(true);
      toast(`Receiver Mode: ${btn.textContent.trim()}`);
    }
  });
});
["setAlwaysOnTop", "setAutoTheater", "setAutoSwitchAudio", "setPreferDolbyAtmos", "setShowTransportControls"].forEach((id) => {
  const sw = $(id);
  if (!sw) return;
  const toggle = () => {
    const next = !sw.classList.contains("on");
    if (id === "setAlwaysOnTop") runtimeSettings.alwaysOnTopMirror = next;
    else if (id === "setAutoTheater") runtimeSettings.autoTheater = next;
    else if (id === "setAutoSwitchAudio") {
      runtimeSettings.autoSwitchAudioOutput = next;
      toast(next ? "Auto-switch to new sound output enabled" : "Auto-switch to new sound output disabled");
    } else if (id === "setPreferDolbyAtmos") {
      runtimeSettings.preferDolbyAtmos = next;
      toast(next ? "Prefer Dolby Atmos enabled" : "Prefer Dolby Atmos disabled");
      if (TAURI) {
        invoke("get_spatial_audio_capabilities").then(updateSpatialAudioUI).catch(() => {});
      }
      persistSettings(true);
      return;
    } else if (id === "setShowTransportControls") {
      runtimeSettings.showTransportControls = next;
      updateTransportControlsVisibility();
      toast(next ? "Transport controls enabled" : "Transport controls hidden");
    }
    persistSettings(false);
  };
  sw.addEventListener("click", toggle);
  sw.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); }
  });
});

const audioSelect = $("setAudioOutputSelect");
if (audioSelect) {
  audioSelect.addEventListener("change", () => {
    runtimeSettings.audioOutputDevice = audioSelect.value || "default";
    persistSettings(true);
    const chosenName = audioSelect.options[audioSelect.selectedIndex]?.text || "Device";
    toast(`Sound output set to: ${chosenName}`);
    if (TAURI) {
      const devId = runtimeSettings.audioOutputDevice === "default" ? null : runtimeSettings.audioOutputDevice;
      invoke("get_spatial_audio_capabilities", { deviceId: devId })
        .then(updateSpatialAudioUI)
        .catch(() => {});
    }
  });
}

const refreshAudioBtn = $("setRefreshAudioBtn");
if (refreshAudioBtn) {
  refreshAudioBtn.addEventListener("click", async () => {
    await refreshAudioDevicesList();
    toast("Audio devices refreshed");
  });
}

/* ============================================================
   backend bridge
   ============================================================ */
async function loadFromBackend() {
  applyEngineState("starting");
  try {
    try {
      const cached = JSON.parse(localStorage.getItem("mirr_settings") || "null");
      if (cached && typeof cached === "object") {
        runtimeSettings = { ...runtimeSettings, ...cached };
      }
    } catch (_) {}
    const name = await invoke("device_name");
    currentBroadcastName = name || "Mirr";
    runtimeSettings.broadcastName = currentBroadcastName;
    const saved = await invoke("get_settings").catch(() => null);
    if (saved) {
      runtimeSettings = {
        ...runtimeSettings,
        broadcastName: saved.broadcastName || currentBroadcastName,
        fps: Number(saved.fps) || 60,
        vsync: !!saved.vsync,
        alwaysOnTopMirror: !!saved.alwaysOnTopMirror,
        autoTheater: !!saved.autoTheater,
        audioOutputDevice: saved.audioOutputDevice || "default",
        autoSwitchAudioOutput: saved.autoSwitchAudioOutput !== false,
        preferDolbyAtmos: saved.preferDolbyAtmos !== false,
        // REMINDER: Default showTransportControls to false on initial setup of the app
        showTransportControls: saved.showTransportControls === true
      };
      currentBroadcastName = runtimeSettings.broadcastName;
    }
    updateTransportControlsVisibility();
    await refreshAudioDevicesList();
    const initialSpatial = await invoke("get_spatial_audio_capabilities").catch(() => null);
    if (initialSpatial) {
      updateSpatialAudioUI(initialSpatial);
    }
    applyTheme(runtimeSettings.bgTheme || "horizon");
    syncSettingsUI();
    $("rxEngineTag").textContent = "· mDNS live";

    refreshDevices = async () => {
      const ap = await invoke("list_airplay_devices").catch(() => []);
      renderAirplayDevices(ap);
    };
    const refreshTargets = async () => {
      const tg = await invoke("list_airplay_targets").catch(() => []);
      renderRooms("rooms", tg, castTo);
    };
    refreshDevices(); refreshTargets();
    // skip polling entirely while the window is hidden — nothing can be seen,
    // and each pass is an IPC round-trip plus a re-render diff
    setInterval(() => { if (!document.hidden) refreshDevices(); }, 5000);
    setInterval(() => { if (!document.hidden) refreshTargets(); }, 8000);

    // Live backend discovery events
    await listen("airplay://devices", (e) => renderAirplayDevices(e.payload || []));

    // Listener first, then snapshot: a peer registering in the gap between the
    // snapshot and the listener attaching would be missed, and since
    // localsend.rs only emits on change, that peer's next (identical)
    // announcement is deduped and it never appears. renderPeers has its own
    // change-guard, so the resulting duplicate render from the snapshot is free.
    await listen("localsend://peers", (e) => renderPeers(e.payload || []));
    renderPeers(await invoke("list_localsend_peers").catch(() => []));
    listen("localsend://received", (e) => {
      const p = e.payload || {};
      const fname = p.name || basename(p.path || "a file");
      recordTransferHistory({
        dir: "in",
        name: fname,
        peer: p.from || "LocalSend peer",
        path: p.path || null
      });
      toast("Received " + fname, false, "Show in folder", () => revealPath(p.path || null));
    });

    // Windows audio output change & auto-switch events
    listen("audio://default-changed", (e) => {
      const payload = e.payload || {};
      if (payload.devices) {
        availableAudioDevices = payload.devices;
        renderAudioDeviceOptions();
      }
      if (payload.device && payload.autoSwitched) {
        toast(`Sound output switched to: ${payload.device.name}`);
      }
      invoke("get_spatial_audio_capabilities").then(updateSpatialAudioUI).catch(() => {});
    });
    listen("audio://device-switched", (e) => {
      const payload = e.payload || {};
      if (payload.device) {
        toast(`Sound output switched to: ${payload.device.name}`);
        renderAudioDeviceOptions();
      }
      invoke("get_spatial_audio_capabilities").then(updateSpatialAudioUI).catch(() => {});
    });
    listen("audio://devices-changed", (e) => {
      const payload = e.payload || {};
      if (payload.devices) {
        availableAudioDevices = payload.devices;
        renderAudioDeviceOptions();
      }
      invoke("get_spatial_audio_capabilities").then(updateSpatialAudioUI).catch(() => {});
    });
    listen("audio://spatial-changed", (e) => {
      const caps = e.payload || {};
      updateSpatialAudioUI(caps);
      if (caps.is_dolby_atmos || caps.isDolbyAtmos) {
        toast(`Dolby Atmos active on ${caps.spatial_technology || "audio endpoint"}`);
      }
    });

    // engine + mirror lifecycle
    listen("airplay://status", (e) => {
      const { state, detail } = e.payload || {};
      if (!state) return;
      if (state === "stopped" || state === "error") {
        // the engine dying ends any audio-only session with it. Clear the
        // flags *before* anything repaints, so no intermediate renderStage()
        // (setMirroring's, setNpMeta's) can leave the stage looking live.
        const hadNp = npActive;
        npActive = false;
        if (hadNp) dacpAvailable = false;
        if (mirroring) setMirroring(false, state, detail); // repaints + renderStage()s
        else applyEngineState(state, detail);
        if (hadNp) { setNpMeta({}); resetNpProgress(); }
        renderStage();
      } else if (!mirroring && !npActive) {
        applyEngineState(state, detail);
      } else if (!mirroring) {
        // an audio-only Now Playing stage owns the copy: engines.rs emits
        // "connecting" on any connection-request line with no session
        // awareness, so painting it here would flash amber "Connecting…" over
        // a live audio session. Only book-keep. Anything non-terminal means
        // the engine is up, so remember it as plain "ready": when the session
        // ends, renderStage()'s else-branch repaints from these two vars and
        // must land on "Ready for AirPlay" / "Discoverable as Mirr", not on a
        // stale "Connecting…" left over from the session that just ended.
        engineState = "ready";
        engineDetail = "";
        syncRxButton();
      }
      refreshDevices();
    });
    listen("mirror://started", () => {
      nav(document.querySelector('[data-view=receive]'));
      setMirroring(true);
      refreshDevices();
    });
    listen("mirror://ended", () => {
      if (mirroring) setMirroring(false);
      refreshDevices();
    });
    // engine found a video window before we ever reported a dockable rect
    // (deep-linked into another view) — surface the receive stage so it docks
    listen("mirror://detected", () => {
      nav(document.querySelector('[data-view=receive]'));
    });

    // audio-only Now Playing lifecycle (mirror wins by construction, both
    // here and in the backend FSM which deactivates NP on usingScreen=1)
    listen("nowplaying://session", (e) => {
      isDemoAudioActive = false;
      npActive = !!(e.payload && e.payload.active);
      if (npActive) {
        const cur = document.querySelector('.view.show');
        if (!cur || (cur.id !== 'airplay2' && cur.id !== 'receive')) {
          nav(document.querySelector('[data-view=receive]'));
        }
      } else {
        setNpMeta({});
        resetNpProgress();
        setNpArt(null);
      }
      renderStage();
      refreshDevices();
    });
    listen("nowplaying://meta", (e) => setNpMeta(e.payload || {}));
    listen("nowplaying://art", (e) => setNpArt((e.payload && e.payload.dataUrl) || null));
    listen("nowplaying://progress", (e) => {
      const p = e.payload || {};
      setNpProgress(Number(p.position) || 0, Number(p.duration) || 0, 0);
    });
    listen("dacp://state", (e) => {
      dacpAvailable = !!(e.payload && e.payload.available);
      renderStage();
    });

    // AirPlay 2 native session events
    listen("airplay2://status", (e) => {
      const p = e.payload || {};
      if (p.dolbyAtmos !== undefined || p.spatialAudio !== undefined) {
        updateSpatialAudioUI(p);
      }
      if (p.state === "streaming") {
        a2ActiveSession = true;
        if ($("a2status")) {
          $("a2status").textContent = "Streaming Lossless";
          $("a2status").className = "tag good";
        }
        if ($("a2wave")) $("a2wave").classList.add("on");
      } else if (p.state === "ready") {
        a2ActiveSession = false;
        setA2RunningState(true);
      } else if (p.state === "stopped") {
        a2ActiveSession = false;
        setA2RunningState(false);
      } else if (p.state === "error") {
        toast("AirPlay 2 error: " + (p.detail || ""), true);
      }
    });

    listen("airplay2://session", (e) => {
      const p = e.payload || {};
      if (p.dolbyAtmos !== undefined || p.spatialAudio !== undefined) {
        updateSpatialAudioUI(p);
      }
      a2ActiveSession = !!p.active;
      np.playing = !!p.active;
      updatePlayIcon(!!p.active);
      const w = $("a2wave");
      if (p.active) {
        if (w) w.classList.add("on");
        if ($("a2status")) {
          $("a2status").textContent = "Streaming Lossless";
          $("a2status").className = "tag good";
        }
        if ($("a2sub")) {
          const rateStr = p.rate ? (p.rate / 1000).toFixed(1) + " kHz" : "44.1 kHz";
          const atmosTag = p.dolbyAtmos ? " · Dolby Atmos Spatial" : "";
          $("a2sub").textContent = `Streaming from ${p.peer || "Apple Device"} · Lossless ${rateStr} PCM${atmosTag}`;
        }
      } else {
        if (w) w.classList.remove("on");
        if ($("a2track")) $("a2track").textContent = "Nothing playing";
        if ($("a2sub")) $("a2sub").textContent = "Waiting for an AirPlay 2 source · 44.1 kHz 16-bit PCM";
        if ($("a2art")) $("a2art").style.backgroundImage = "none";
        if ($("a2bar")) $("a2bar").style.width = "0%";
        if ($("a2elapsed")) $("a2elapsed").textContent = "0:00";
        if ($("a2duration")) $("a2duration").textContent = "0:00";
        if ($("a2status")) {
          $("a2status").textContent = a2Running ? ("Ready · " + (currentBroadcastName || "Mirr") + " Audio") : "Engine Stopped";
          $("a2status").className = a2Running ? "tag good" : "tag";
        }
      }
    });

    listen("airplay2://metadata", (e) => {
      const p = e.payload || {};
      if ($("a2track")) $("a2track").textContent = p.title || "Unknown Track";
      if ($("a2sub")) {
        const parts = [];
        if (p.artist) parts.push(p.artist);
        if (p.album) parts.push(p.album);
        $("a2sub").textContent = parts.join(" — ") || "Lossless AirPlay 2 Stream";
      }
    });

    listen("airplay2://artwork", (e) => {
      const p = e.payload || {};
      const el = $("a2art");
      if (el) {
        if (p.dataUrl) {
          el.style.backgroundImage = `url("${p.dataUrl}")`;
          el.style.backgroundSize = "cover";
          el.style.backgroundPosition = "center";
        } else {
          el.style.backgroundImage = "none";
        }
      }
    });

    listen("airplay2://progress", (e) => {
      const p = e.payload || {};
      const elap = Math.floor(p.elapsedSecs || p.elapsed || 0);
      const dur = Math.floor(p.durationSecs || p.duration || 0);
      if ($("a2elapsed")) $("a2elapsed").textContent = fmtTime(elap);
      if ($("a2duration")) $("a2duration").textContent = fmtTime(dur);
      if ($("a2bar")) {
        const pct = dur > 0 ? Math.min(100, Math.max(0, (elap / dur) * 100)) : (p.percent ? p.percent * 100 : 0);
        $("a2bar").style.width = pct.toFixed(1) + "%";
      }
    });

    listen("airplay2://volume", (e) => {
      const p = e.payload || {};
      if (typeof p.percent === "number") {
        updateVolUi(p.percent * 100);
      }
    });

    listen("airplay2://paused", (e) => {
      const p = e.payload || {};
      const isPaused = !!p.paused;
      np.playing = !isPaused;
      updatePlayIcon(!isPaused);
      const w = $("a2wave");
      if (w) {
        if (isPaused) w.classList.remove("on");
        else if (a2ActiveSession) w.classList.add("on");
      }
    });

    listen("nowplaying://paused", (e) => {
      const p = e.payload || {};
      const isPaused = !!p.paused;
      np.playing = !isPaused;
      updatePlayIcon(!isPaused);
    });

    // Query initial AirPlay 2 state
    const a2St = await invoke("airplay2_state").catch(() => null);
    if (a2St && typeof a2St.running === "boolean") {
      setA2RunningState(a2St.running);
    }

    // events emitted before our listeners attached are gone — ask for the
    // current state explicitly
    const st = await invoke("receiver_state").catch(() => null);
    if (st && st.state && !mirroring) applyEngineState(st.state, st.detail);
    if (await invoke("mirror_active").catch(() => false)) setMirroring(true);

    const npSt = await invoke("nowplaying_state").catch(() => null);
    if (npSt) {
      npActive = !!npSt.active;
      setNpMeta(npSt.meta || {});
      if (npSt.artDataUrl) setNpArt(npSt.artDataUrl);
      if (npSt.progress) {
        setNpProgress(Number(npSt.progress.position) || 0, Number(npSt.progress.duration) || 0, Number(npSt.progress.ageMs) || 0);
      }
      renderStage();
    }
    const dacpSt = await invoke("dacp_state").catch(() => null);
    if (dacpSt) { dacpAvailable = !!dacpSt.available; renderStage(); }
  } catch (err) {
    console.warn("backend load failed, using demo", err);
    loadDemo();
  }
}

function loadDemo() {
  try {
    const cached = JSON.parse(localStorage.getItem("mirr_settings") || "null");
    if (cached && typeof cached === "object") {
      runtimeSettings = { ...runtimeSettings, ...cached };
      currentBroadcastName = runtimeSettings.broadcastName || "Mirr";
    }
  } catch (_) {}
  syncSettingsUI();
  refreshAudioDevicesList();
  updateSpatialAudioUI({
    is_spatial_supported: true,
    is_dolby_atmos: true,
    spatial_technology: "Dolby Atmos (Hardware / DAX3)",
    max_channels: 8
  });
  applyEngineState("ready");
  $("rxState").textContent = "Idle — demo";
  renderAirplayDevices(DEMO.airplayDevices);
  renderRooms("rooms", DEMO.castTargets, castTo);
  renderPeers(DEMO.localsendPeers.map((p, i) => ({ ...p, fingerprint: "demo" + i })));
  $("lsScan").textContent = DEMO.localsendPeers.length ? `· ${DEMO.localsendPeers.length} nearby` : "· scanning…";
}

if (TAURI) { loadFromBackend(); } else { loadDemo(); }

/* Suspend ambient animation whenever nobody is looking, or live video owns the
   stage. This is not cosmetic: the window is transparent + acrylic, so every
   animated frame makes DWM recomposite the whole window behind it. */
function updateIdleBg() {
  document.body.classList.toggle("idlebg", document.hidden || !windowFocused || mirroring);
}
/* Idle-motion governor. The radar/pulse animations are the only thing running
   when the app sits waiting for a device, and on this transparent+acrylic
   window an active animation costs ~8% GPU + ~15% DWM no matter how few frames
   it submits. So after a stretch of no interaction we stop them outright; any
   activity brings them straight back. Rescheduling the timeout is throttled
   while already active so 250-1000 Hz pointermove events don't churn the
   timer heap on every pixel of cursor movement. */
function goActive() {
  const now = Date.now();
  if (document.body.classList.contains("calm")) {
    document.body.classList.remove("calm");
  } else if (calmTimer !== null && now - lastCalmReset < 400) {
    return;
  }
  lastCalmReset = now;
  clearTimeout(calmTimer);
  calmTimer = setTimeout(() => {
    calmTimer = null;
    document.body.classList.add("calm");
  }, CALM_AFTER_MS);
}
["pointermove", "pointerdown", "keydown", "wheel"].forEach(ev =>
  window.addEventListener(ev, goActive, { passive: true }));
window.addEventListener("focus", goActive);
goActive();

document.addEventListener("visibilitychange", () => {
  updateIdleBg();
  if (!document.hidden) {
    if (npActive) renderNpProgress();
    reportRectSoon();
  }
});
window.addEventListener("blur", () => { windowFocused = false; updateIdleBg(); });
window.addEventListener("focus", () => { windowFocused = true; updateIdleBg(); });
updateIdleBg();

/* deep-link: open a specific view via #hash (e.g. index.html#localsend) */
function applyHash() {
  const v = (location.hash || "").replace("#", "");
  const btn = document.querySelector('.navbtn[data-view="' + v + '"]');
  if (btn) nav(btn);
}
window.addEventListener("hashchange", applyHash);
applyHash();
