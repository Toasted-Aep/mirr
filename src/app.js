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

const $ = (id) => document.getElementById(id);
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
function toast(msg, bad = false) {
  const el = document.createElement("div");
  el.className = "toast" + (bad ? " bad" : "");
  el.textContent = msg;
  $("toasts").appendChild(el);
  setTimeout(() => { el.style.opacity = "0"; el.style.transition = "opacity .4s"; }, 3600);
  setTimeout(() => el.remove(), 4100);
}

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
  $("vsub").textContent = titles[v][1];
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
  airplayDevices: [
    { name: "iPhone 16 Pro", meta: "Nearby", online: true, type: "iphone" },
    { name: "MacBook Pro", meta: '14" · macOS 26', online: true, type: "mac" },
    { name: "iPad Air", meta: "Last seen 2h ago", online: false, type: "ipad" }
  ],
  castTargets: [
    { name: "Living Room HomePod", meta: "AirPlay 2 · paired", on: true, vol: 82 },
    { name: "Kitchen — AirPort Express", meta: "Classic AirPlay · RAOP", on: false, vol: 55 },
    { name: "Office Speakers", meta: "AirPlay 2 · grouped", on: false, vol: 40 }
  ],
  a2rooms: [
    { name: "This PC (Mirr)", meta: "Lossless · master", on: true, vol: 75 },
    { name: "Living Room HomePod", meta: "Grouped · in sync", on: true, vol: 60 },
    { name: "Office Speakers", meta: "Grouped · +12 ms buffer", on: false, vol: 45 }
  ],
  localsendPeers: [
    { alias: "iPhone 16 Pro", model: "iOS 26", type: "iphone" },
    { alias: "MacBook Pro", model: "macOS 26", type: "mac" },
    { alias: "Pixel 9", model: "Android 16", type: "android" }
  ]
};

/* ============================================================
   AirPlay receive — engine state machine + native video docking
   ============================================================ */
let engineState = "starting"; // starting | ready | connecting | mirroring | stopped | error
let engineDetail = ""; // last detail string, so a repaint can't lose an error message
let mirroring = false;
let zoom = 100;

/* ---------- audio-only Now Playing session ---------- */
let npActive = false;
let dacpAvailable = false;
let npMeta = { title: "", artist: "", album: "", hasData: false, rev: -1 };
// local 1 Hz elapsed-time ticker: extrapolated from the last progress
// baseline using the wall clock, re-baselined on every progress event
const np = { baseSec: null, durSec: null, baseWall: 0, playing: false };
// same track info the AirPlay-2 demo button uses — reused for the
// Now Playing stage's "Simulate audio stream" demo path
const DEMO_TRACK = { title: "Resonance — Ambient Mix", artist: "Aurora Fields", album: "Night Drift" };

function setDisco(txt, dotCls) {
  $("disco").textContent = txt;
  $("discoDot").className = "dot " + dotCls;
}

function applyEngineState(state, detail) {
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
      sub.innerHTML = "On your iPhone or Mac, open <b>Screen Mirroring</b> and pick <b>Mirr</b>";
      st.textContent = "Idle — waiting for a device";
      setDisco("Discoverable as Mirr", "pulse");
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
  $("npTransport").hidden = !dacpAvailable;
  $("mTransport").hidden = !(mirroring && dacpAvailable);
  if (showNp) {
    const title = npMeta.hasData && npMeta.title ? npMeta.title : "Audio session live";
    $("rxState").textContent = "Audio session · " + title;
    setDisco("Audio session", "pulse");
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
setInterval(renderNpProgress, 1000);

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
  const art = $("npArt"), bg = $("npBg");
  if (dataUrl) {
    art.style.backgroundImage = `url("${dataUrl}")`;
    art.classList.add("has-art");
    bg.style.backgroundImage = `url("${dataUrl}")`;
    bg.classList.add("has-art");
  } else {
    art.style.backgroundImage = "";
    art.classList.remove("has-art");
    bg.style.backgroundImage = "";
    bg.classList.remove("has-art");
  }
}

function setNpMeta(meta) {
  meta = meta || {};
  const revChanged = meta.rev !== npMeta.rev;
  npMeta = { title: meta.title || "", artist: meta.artist || "", album: meta.album || "",
    hasData: !!meta.hasData, rev: meta.rev };
  $("npTitle").textContent = npMeta.hasData && npMeta.title ? npMeta.title : "Audio from your device";
  let artistLine = "";
  if (npMeta.hasData) {
    artistLine = npMeta.artist || "";
    if (npMeta.album) artistLine += artistLine ? " · " + npMeta.album : npMeta.album;
  }
  $("npArtist").textContent = artistLine || "Waiting for track info…";
  // -md rewrites happen on every SETUP; art always clears to placeholder
  // here and repaints only when the next nowplaying://art event lands
  // (stale-art caveat from R1) — never assume the old art still matches.
  setNpArt(null);
  if (revChanged) resetNpProgress();
  renderStage();
}

function updatePlayIcon(playing) {
  const path = playing
    ? '<path d="M8 6h3v12H8zM13 6h3v12h-3z"/>'
    : '<path d="M8 5v14l11-7Z"/>';
  $("npPlayIcon").innerHTML = path;
  $("mPlayIcon").innerHTML = path;
}

/* dacp_command failures are always quiet — a non-2xx just means the app on
   the phone doesn't support this command, not an error worth a toast */
function sendTransport(cmd) {
  if (!TAURI) { toast("Demo mode — connect a real device for transport control"); return; }
  invoke("dacp_command", { cmd }).then((code) => {
    if (cmd === "playpause" && code >= 200 && code < 300) {
      // fold the already-ticked time into the baseline first, otherwise a
      // pause snaps the bar back to the last progress line (the phone
      // re-baselines us shortly after anyway)
      if (np.playing && np.baseSec != null)
        np.baseSec = Math.min(np.durSec ?? Infinity, np.baseSec + (Date.now() - np.baseWall) / 1000);
      np.playing = !np.playing;
      np.baseWall = Date.now();
      updatePlayIcon(np.playing);
      renderNpProgress();
    }
  }).catch(() => {});
}
["npPrev", "mPrev"].forEach(id => $(id).addEventListener("click", () => sendTransport("previtem")));
["npNext", "mNext"].forEach(id => $(id).addEventListener("click", () => sendTransport("nextitem")));
["npPlay", "mPlay"].forEach(id => $(id).addEventListener("click", () => sendTransport("playpause")));

let volDebounce = null;
function sendVolume(pct) {
  if (!TAURI) return;
  clearTimeout(volDebounce);
  volDebounce = setTimeout(() => {
    invoke("dacp_set_volume", { pct: Number(pct) }).catch(() => {});
  }, 150);
}
$("npVol").addEventListener("input", (e) => { $("mVol").value = e.target.value; sendVolume(e.target.value); });
$("mVol").addEventListener("input", (e) => { $("npVol").value = e.target.value; sendVolume(e.target.value); });

/* media keys — trivial to wire, silent no-op when unsupported */
window.addEventListener("keydown", (e) => {
  if (e.key === "MediaPlayPause" || e.key === "MediaPlay" || e.key === "MediaPause") sendTransport("playpause");
  else if (e.key === "MediaTrackNext") sendTransport("nextitem");
  else if (e.key === "MediaTrackPrevious") sendTransport("previtem");
});

/* enter/leave the live-mirror layout. `nextState` is the engine state to
   show after leaving (e.g. "stopped" when the engine died mid-mirror). */
function setMirroring(on, nextState, detail) {
  if (mirroring === on && !nextState) { /* idempotent */ }
  mirroring = on;
  document.body.classList.toggle("mirroring", on);
  $("theaterBtn").hidden = !on;
  $("zoomCtl").hidden = !on;
  document.body.classList.toggle("idlebg", on || document.hidden);
  if (on) {
    applyEngineState("mirroring");
  } else {
    document.body.classList.remove("theater");
    setZoom(100, true);
    applyEngineState(nextState || "ready", detail);
  }
  renderStage();
  reportRectSoon();
}

/* --- rect reporting: keep the native video glued to #videoHost --- */
let lastRect = "";
function reportRect() {
  if (!TAURI) return;
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
  setInterval(reportRect, 250);
  window.addEventListener("resize", reportRectSoon);
  document.querySelector(".stage").addEventListener("scroll", reportRect, { passive: true });
  reportRectSoon();
}

/* --- digital zoom --- */
function setZoom(z, silent) {
  zoom = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, z));
  $("zoomLbl").textContent = zoom + "%";
  if (!silent) reportRect();
}
$("zoomIn").addEventListener("click", () => setZoom(zoom + ZOOM_STEP));
$("zoomOut").addEventListener("click", () => setZoom(zoom - ZOOM_STEP));
$("zoomLbl").addEventListener("click", () => setZoom(100));

/* --- theater mode --- */
$("theaterBtn").addEventListener("click", () => {
  document.body.classList.toggle("theater");
  reportRectSoon();
});
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && document.body.classList.contains("theater")) {
    document.body.classList.remove("theater");
    reportRectSoon();
    return;
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
    $("rxEmpty").classList.toggle("hide", on);
    $("rxState").textContent = on ? "Connected · iPhone 16 Pro · 1080p60 · ~42 ms latency" : "Idle — demo";
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
  npActive = true;
  dacpAvailable = true;
  setNpMeta({ title: DEMO_TRACK.title, artist: DEMO_TRACK.artist, album: DEMO_TRACK.album, hasData: true, rev: Date.now() });
  setNpProgress(12, 222, 0);
  updatePlayIcon(true);
  $("npDemoBtn").textContent = "Stop audio stream";
  renderStage();
}
function stopDemoNowPlaying() {
  if (!npActive) return;
  npActive = false;
  dacpAvailable = false;
  resetNpProgress();
  $("npDemoBtn").textContent = "Simulate audio stream";
  renderStage();
  $("rxState").textContent = "Idle — demo"; // same override loadDemo() uses
}

let lastDevicesJson = "";
function renderAirplayDevices(list) {
  const json = JSON.stringify(list);
  if (json === lastDevicesJson) return;
  lastDevicesJson = json;
  const g = $("rxDevices"); g.innerHTML = "";
  if (!list.length) {
    g.innerHTML = `<div class="card tile"><div class="ic">${ICON.generic}</div><div class="name">Nothing seen yet</div>
      <div class="meta">Apple devices on this network appear here</div></div>`;
    return;
  }
  list.forEach(d => {
    const el = document.createElement("div"); el.className = "card tile";
    el.innerHTML = `<div class="ic">${pickIcon(d.type || d.name)}</div><div class="name">${esc(d.name)}</div>
      <div class="meta">${esc(d.meta || "")}</div>
      <div class="status"><span class="dot ${d.online ? "" : "off"}"></span>${d.online ? "On the network" : "Offline"}</div>`;
    g.appendChild(el);
  });
}

/* ============================================================
   equalizer bars
   ============================================================ */
function buildWave(id) {
  const w = $(id);
  for (let i = 0; i < 22; i++) {
    const b = document.createElement("i");
    b.style.animationDelay = (i * 0.06) + "s";
    b.style.animationDuration = (0.7 + ((i * 7919) % 100) / 140) + "s";
    w.appendChild(b);
  }
}
buildWave("a2wave"); buildWave("castwave"); buildWave("npWave");

/* ---------- AirPlay 2 ---------- */
$("a2btn").addEventListener("click", function () {
  if (TAURI) {
    invoke("start_airplay2_receiver").then(
      () => toast("AirPlay 2 engine started"),
      (e) => toast(String(e), true)
    );
    return;
  }
  const t = $("a2track"), s = $("a2sub"), w = $("a2wave");
  if (t.textContent === "Nothing playing") {
    t.textContent = DEMO_TRACK.title;
    s.textContent = "From iPhone · ALAC 24-bit/48 kHz · 3 rooms in sync";
    w.classList.add("on"); this.textContent = "Stop";
  } else {
    t.textContent = "Nothing playing";
    s.textContent = "Waiting for an AirPlay 2 source · lossless ALAC up to 24-bit/48 kHz";
    w.classList.remove("on"); this.textContent = "Simulate stream";
  }
});
if (TAURI) $("a2btn").textContent = "Start AirPlay 2 engine";

const roomCache = {};
function renderRooms(targetId, list, onToggle) {
  const json = JSON.stringify(list);
  if (roomCache[targetId] === json) return;
  roomCache[targetId] = json;
  const rc = $(targetId); rc.innerHTML = "";
  list.forEach(r => {
    const el = document.createElement("div"); el.className = "room tile";
    el.innerHTML = `<div class="ricon">${ICON.speaker}</div>
      <div class="st" style="flex:1"><div class="rname">${esc(r.name)}</div><div class="rmeta">${esc(r.meta || "")}</div></div>
      <div class="slider" style="max-width:120px"><input type="range" min="0" max="100" value="${Number(r.vol ?? 60)}"></div>
      <div class="switch ${r.on ? "on" : ""}"></div>`;
    const sw = el.querySelector(".switch");
    sw.addEventListener("click", () => {
      sw.classList.toggle("on");
      if (sw.classList.contains("on") && onToggle) onToggle(r);
    });
    rc.appendChild(el);
  });
}

/* ---------- AirCast ---------- */
$("castPlay").addEventListener("click", function () {
  if (TAURI) { toast("Pick a speaker below to start casting"); return; }
  this.textContent = this.textContent === "▶ Stream" ? "⏸ Streaming" : "▶ Stream";
});
function castTo(target) {
  if (!TAURI) return;
  invoke("start_aircast", { targetIp: target.addr || "" }).then(
    () => toast(`Casting to ${target.name}`),
    (e) => toast(String(e), true)
  );
}

/* ============================================================
   LocalSend
   ============================================================ */
let peerCache = [];
let selectedPeer = null;

function renderPeers(list) {
  peerCache = list;
  if (selectedPeer && !list.some(p => p.fingerprint === selectedPeer.fingerprint)) selectedPeer = null;
  const pl = $("lsPeers"); pl.innerHTML = "";
  if (!list.length) {
    pl.innerHTML = `<div class="peer-row tile"><div class="pic">${ICON.generic}</div><div><div class="pn">No devices yet</div><div class="pm">Open LocalSend on another device on this network</div></div></div>`;
    $("lsScan").textContent = "· scanning…";
    return;
  }
  $("lsScan").textContent = `· ${list.length} nearby`;
  list.forEach(p => {
    const el = document.createElement("div"); el.className = "peer-row tile";
    if (selectedPeer && selectedPeer.fingerprint === p.fingerprint) el.classList.add("sel");
    el.innerHTML = `<div class="pic">${pickIcon(p.type || p.model || p.alias)}</div>
      <div style="flex:1"><div class="pn">${esc(p.alias)}</div><div class="pm">${esc(p.model || "")}</div></div>
      <button class="btn">Send</button>`;
    el.querySelector("button").addEventListener("click", e => { e.stopPropagation(); pickAndSend(p); });
    el.addEventListener("click", () => {
      selectedPeer = p;
      pl.querySelectorAll(".peer-row").forEach(r => r.classList.remove("sel"));
      el.classList.add("sel");
      $("dropHint").textContent = `sends to ${p.alias}`;
    });
    pl.appendChild(el);
  });
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

async function sendPath(peer, path) {
  showTransfer(basename(path), peer.alias, true);
  try {
    await invoke("localsend_send", { targetIp: peer.ip, port: peer.port, path });
    finishTransfer(true);
    return true;
  } catch (e) {
    finishTransfer(false, String(e));
    return false;
  }
}

async function pickAndSend(peer) {
  selectedPeer = peer;
  if (!TAURI) { fakeTransfer(peer.alias); return; }
  const file = await window.__TAURI__.dialog.open({ multiple: false, title: "Send a file with LocalSend" }).catch(() => null);
  if (!file) return;
  sendPath(peer, typeof file === "string" ? file : file.path || String(file));
}

function fakeTransfer(dev) {
  const names = ["photo.heic", "presentation.pdf", "clip.mov", "design.fig", "notes.md"];
  showTransfer(names[Math.floor(Math.random() * names.length)], dev, false);
  const bar = $("tfBar"), pct = $("tfPct");
  let p = 0;
  const iv = setInterval(() => {
    p += Math.random() * 9 + 3;
    if (p >= 100) { p = 100; clearInterval(iv); pct.textContent = "✓ Delivered"; }
    bar.style.width = p + "%";
    if (p < 100) pct.textContent = "Sending… " + Math.floor(p) + "%";
  }, 160);
}

/* drop zone — Tauri delivers real paths via its drag-drop event.
   Drops only count on the LocalSend view, and only with a clear target
   (the selected peer, or the single peer when there is exactly one). */
const drop = $("drop");
drop.addEventListener("dragover", e => { e.preventDefault(); drop.classList.add("hot"); });
drop.addEventListener("dragleave", () => drop.classList.remove("hot"));
drop.addEventListener("drop", e => {
  e.preventDefault(); drop.classList.remove("hot");
  if (!TAURI) fakeTransfer((selectedPeer && selectedPeer.alias) || "the selected device");
});
if (listen) {
  const onLocalsendView = () => $("localsend").classList.contains("show");
  listen("tauri://drag-enter", () => { if (onLocalsendView()) drop.classList.add("hot"); });
  listen("tauri://drag-leave", () => drop.classList.remove("hot"));
  listen("tauri://drag-drop", async (e) => {
    drop.classList.remove("hot");
    if (!onLocalsendView()) return;
    const paths = (e.payload && e.payload.paths) || [];
    if (!paths.length) return;
    const peer = selectedPeer || (peerCache.length === 1 ? peerCache[0] : null);
    if (!peer) { toast("Pick a device first — click one in the list", true); return; }
    for (const p of paths) {
      if (!await sendPath(peer, p)) break;
    }
  });
}

/* ============================================================
   backend bridge
   ============================================================ */
async function loadFromBackend() {
  applyEngineState("starting");
  try {
    const name = await invoke("device_name");
    $("setName").textContent = name;
    $("rxEngineTag").textContent = "· mDNS live";

    const refreshDevices = async () => {
      const ap = await invoke("list_airplay_devices").catch(() => []);
      renderAirplayDevices(ap);
    };
    const refreshTargets = async () => {
      const tg = await invoke("list_airplay_targets").catch(() => []);
      renderRooms("rooms", tg, castTo);
    };
    refreshDevices(); refreshTargets();
    setInterval(refreshDevices, 5000);
    setInterval(refreshTargets, 8000);

    renderPeers(await invoke("list_localsend_peers").catch(() => []));
    listen("localsend://peers", (e) => renderPeers(e.payload || []));
    listen("localsend://received", (e) => toast("Received " + basename((e.payload || {}).path || "a file")));

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
    });
    listen("mirror://started", () => {
      nav(document.querySelector('[data-view=receive]'));
      setMirroring(true);
    });
    listen("mirror://ended", () => { if (mirroring) setMirroring(false); });
    // engine found a video window before we ever reported a dockable rect
    // (deep-linked into another view) — surface the receive stage so it docks
    listen("mirror://detected", () => {
      nav(document.querySelector('[data-view=receive]'));
    });

    // audio-only Now Playing lifecycle (mirror wins by construction, both
    // here and in the backend FSM which deactivates NP on usingScreen=1)
    listen("nowplaying://session", (e) => {
      npActive = !!(e.payload && e.payload.active);
      if (npActive) {
        nav(document.querySelector('[data-view=receive]'));
      } else {
        setNpMeta({});
        resetNpProgress();
      }
      renderStage();
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

    renderRooms("a2rooms", DEMO.a2rooms); // illustration until AirPlay-2 engine ships
  } catch (err) {
    console.warn("backend load failed, using demo", err);
    loadDemo();
  }
}

function loadDemo() {
  applyEngineState("ready");
  $("rxState").textContent = "Idle — demo";
  renderAirplayDevices(DEMO.airplayDevices);
  renderRooms("rooms", DEMO.castTargets);
  renderRooms("a2rooms", DEMO.a2rooms);
  renderPeers(DEMO.localsendPeers.map((p, i) => ({ ...p, fingerprint: "demo" + i })));
  $("lsScan").textContent = "· demo peers";
}

if (TAURI) { loadFromBackend(); } else { loadDemo(); }

/* pause ambient animation when the window is hidden or a mirror is live */
document.addEventListener("visibilitychange", () => {
  document.body.classList.toggle("idlebg", document.hidden || mirroring);
});

/* deep-link: open a specific view via #hash (e.g. index.html#localsend) */
function applyHash() {
  const v = (location.hash || "").replace("#", "");
  const btn = document.querySelector('.navbtn[data-view="' + v + '"]');
  if (btn) nav(btn);
}
window.addEventListener("hashchange", applyHash);
applyHash();
