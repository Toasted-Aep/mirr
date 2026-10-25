//! Audio-only Now Playing session.
//!
//! Watches UxPlay's -md/-ca export files (paths from `engines::md_path` /
//! `engines::ca_path`) and parses stdout markers (ct=/usingScreen SETUP
//! lines, `audio progress` lines, `coverart size .. written to` lines,
//! connection-end markers) into events:
//!   nowplaying://session  { active }
//!   nowplaying://meta     { title, artist, album, hasData, rev }
//!   nowplaying://art      { dataUrl }            // null = placeholder/none
//!   nowplaying://progress { position, duration, remaining }  // seconds

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::engines;

/// Backend session snapshot. Guarded by a single mutex — every field is read
/// by `state()` for a late-attaching frontend.
struct NpState {
    active: bool,
    title: String,
    artist: String,
    album: String,
    has_data: bool,
    /// Monotonic track-boundary counter; the frontend resets its progress bar
    /// to indeterminate whenever this changes.
    rev: u64,
    art_data_url: Option<String>,
    /// (position, duration, parsed_at) from the last stdout progress line.
    progress: Option<(u64, u64, Instant)>,
}

static SESSION: Mutex<NpState> = Mutex::new(NpState {
    active: false,
    title: String::new(),
    artist: String::new(),
    album: String::new(),
    has_data: false,
    rev: 0,
    art_data_url: None,
    progress: None,
});

/// The poll thread is spawned at most once for the process lifetime.
static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
/// Bumped on every SETUP / deactivation. A pending quiescence timer captures
/// this and only fires if it is unchanged (no re-SETUP happened meanwhile).
static GEN: AtomicU64 = AtomicU64::new(0);
/// Generation for which a quiescence timer is already in flight (0 = none),
/// avoiding spawning duplicate timer threads on burst teardown log lines.
static QUIESCE_GEN: AtomicU64 = AtomicU64::new(0);
/// Content-dedupe keys shared by the poll thread and the log-line art poke.
static MD_HASH: AtomicU64 = AtomicU64::new(0);
static CA_HASH: AtomicU64 = AtomicU64::new(0);
/// Cover art that landed via the immediate stdout poke (`reread_art`) before
/// the 300 ms poller observed the accompanying `-md` rewrite for the same
/// track. Consumed once by `handle_md` so `emit_meta`'s clear-on-meta rule
/// doesn't wipe art that arrived milliseconds ahead of its metadata.
static EARLY_ART: Mutex<Option<(Instant, Vec<u8>)>> = Mutex::new(None);

/// uxplay resets -md to this exact content ("no data\n" + strlen+1 trailing
/// NUL) on every SETUP; pre-seeding MD_HASH with it on activate suppresses the
/// otherwise-duplicate hasData:false emit from the poller.
const RESET_MD: &[u8] = b"no data\n\0";
/// Dedupe sentinel for the 95-byte 1x1 placeholder PNG.
const PLACEHOLDER_KEY: u64 = 95;

/// Idempotent: spawn the -md/-ca poll thread (300 ms stat cadence while a
/// session is active, 2 s while idle; 80 ms debounce re-read — writes are
/// non-atomic). Called by `engines::start_airplay_receiver` right after the
/// engine spawns.
pub fn ensure_watcher(app: AppHandle) {
    if WATCHER_STARTED.swap(true, Ordering::SeqCst) {
        return; // already running for this process
    }
    std::thread::spawn(move || {
        let md = engines::md_path();
        let ca = engines::ca_path();
        // (mtime_nanos, len) — a cheap "did the file move" signature so we only
        // pay the read + hash when the stat actually changes.
        let mut md_sig: Option<(u128, u64)> = None;
        let mut ca_sig: Option<(u128, u64)> = None;
        loop {
            // Cadence from state, NOT a constant. This thread outlives the
            // engine (there is no exit path), and both handle_md and handle_ca
            // no-op unless the session is active — so at a constant 300 ms the
            // idle app paid two stat syscalls 3.3x/second forever, for nothing,
            // at a period short enough to defeat Windows timer coalescing.
            // Fast while a real session is running (metadata must feel live),
            // slow otherwise. Do not "simplify" this back to one interval.
            let active = SESSION.lock().unwrap().active;
            std::thread::sleep(Duration::from_millis(if active { 300 } else { 2000 }));

            let sig = file_sig(&md);
            if sig != md_sig {
                md_sig = sig;
                if sig.is_some() {
                    // Writes are open-truncate-write-close; let a torn write settle.
                    std::thread::sleep(Duration::from_millis(80));
                    // Keep the PRE-read sig: re-stating here would record a
                    // signature newer than `bytes`, so a rewrite landing between
                    // the read and the stat would never be re-read. The MD_HASH
                    // dedupe absorbs the redundant re-read this costs instead.
                    if let Ok(bytes) = std::fs::read(&md) {
                        handle_md(&app, &bytes);
                    }
                }
            }

            let sig = file_sig(&ca);
            if sig != ca_sig {
                ca_sig = sig;
                if sig.is_some() {
                    std::thread::sleep(Duration::from_millis(80));
                    if let Ok(bytes) = std::fs::read(&ca) {
                        handle_ca(&app, &bytes); // pre-read sig kept, as above
                    }
                }
            }
        }
    });
}

/// Fed every trimmed, lossy-UTF-8, ORIGINAL-CASE stdout/stderr line by
/// `engines::spawn_log_reader`. Lowercase locally as needed.
pub fn on_log_line(app: &AppHandle, line: &str) {
    let l = line.to_lowercase();

    // --- SETUP line: 'ct=%d spf=%d usingScreen=%d isMedia=%d  audioFormat=...'
    // Token-based only — the printf has a double space and may carry a logger
    // prefix, so an exact-format match would be fragile.
    if l.split_whitespace().any(|t| t.starts_with("usingscreen=")) {
        let toks = l.split_whitespace();
        let mut ct2 = false;
        let mut mirror = false;
        for t in toks {
            match t {
                "ct=2" => ct2 = true,
                "usingscreen=1" => mirror = true,
                _ => {}
            }
        }
        if mirror {
            // Mirror precedence: client switched audio -> mirror mid-session.
            deactivate(app);
        } else if ct2 {
            // Audio-only ALAC SETUP (also the re-SETUP path).
            activate(app);
        }
        return;
    }

    // --- progress line (stdout only; not in the -md file)
    if l.contains("audio progress") {
        if let Some((pos, dur)) = parse_progress(line) {
            let active = {
                let mut s = SESSION.lock().unwrap();
                if s.active {
                    s.progress = Some((pos, dur, Instant::now()));
                }
                s.active
            };
            if active {
                let remaining = dur.saturating_sub(pos);
                let _ = app.emit(
                    "nowplaying://progress",
                    json!({ "position": pos, "duration": dur, "remaining": remaining }),
                );
            }
        }
        return;
    }

    // --- cover-art write: poke an immediate re-read (beats the 300 ms poll)
    if l.contains("coverart size") && l.contains("written to") {
        reread_art(app);
        return;
    }

    // --- network loss: immediate end
    if l.contains("lost connection with client") || l.contains("missed client feedback") {
        deactivate(app);
        return;
    }

    // --- graceful connection teardown: arm the quiescence timer. AirPlay
    // clients hold several TCP connections, so we only end the audio session
    // if no re-SETUP (gen change) happens within ~1.2 s.
    if l.contains("connection closed") || l.contains("removing connection") {
        if SESSION.lock().unwrap().active {
            let gen_at = GEN.load(Ordering::SeqCst);
            if QUIESCE_GEN.swap(gen_at, Ordering::SeqCst) == gen_at {
                return; // timer already armed for this generation
            }
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1200));
                // Hold TRANSITION across the check AND the deactivate so an
                // activate() can't slip in between them. std Mutex is not
                // reentrant, so call the inner form, never `deactivate`.
                let _t = TRANSITION.lock().unwrap();
                if GEN.load(Ordering::SeqCst) == gen_at && SESSION.lock().unwrap().active {
                    deactivate_inner(&app);
                }
            });
        }
    }
}

/// The engine stopped (user stop or pipe EOF) — force the session inactive.
pub fn on_engine_stop(app: &AppHandle) {
    deactivate(app);
}

pub fn is_active() -> bool {
    SESSION.lock().unwrap().active
}

/// Snapshot for a late-attaching frontend (same pattern as receiver_state).
/// `progress` is null or { position, duration, remaining, ageMs }.
pub fn state() -> serde_json::Value {
    let s = SESSION.lock().unwrap();
    let progress = s.progress.map(|(pos, dur, at)| {
        json!({
            "position": pos,
            "duration": dur,
            "remaining": dur.saturating_sub(pos),
            "ageMs": at.elapsed().as_millis() as u64,
        })
    });
    json!({
        "active": s.active,
        "meta": {
            "title": s.title,
            "artist": s.artist,
            "album": s.album,
            "hasData": s.has_data,
            "rev": s.rev,
        },
        "artDataUrl": s.art_data_url,
        "progress": progress,
    })
}

// ── state transitions ──────────────────────────────────────────────────────

/// Serializes whole activate/deactivate transitions (mutation *and* emits) so a
/// quiescence timer's GEN check cannot be invalidated between the check and the
/// emit. Held only around the transitions themselves — SESSION is still taken
/// for short mutations inside, so emit_meta's own lock cannot deadlock.
static TRANSITION: Mutex<()> = Mutex::new(());

fn activate(app: &AppHandle) {
    let _t = TRANSITION.lock().unwrap();
    activate_inner(app);
}

fn deactivate(app: &AppHandle) {
    let _t = TRANSITION.lock().unwrap();
    deactivate_inner(app);
}

/// Inactive/Active -> Active. Resets track state (uxplay just reset -md/-ca on
/// this same SETUP) and re-emits the fresh-session events.
/// Caller must hold TRANSITION.
fn activate_inner(app: &AppHandle) {
    GEN.fetch_add(1, Ordering::SeqCst);
    QUIESCE_GEN.store(0, Ordering::SeqCst);
    // Seed the dedupe keys with the SETUP-reset content so the poller doesn't
    // re-emit the placeholder/no-data we're about to emit here.
    MD_HASH.store(hash_bytes(RESET_MD), Ordering::SeqCst);
    CA_HASH.store(PLACEHOLDER_KEY, Ordering::SeqCst);
    *EARLY_ART.lock().unwrap() = None;
    {
        let mut s = SESSION.lock().unwrap();
        s.active = true;
        s.title.clear();
        s.artist.clear();
        s.album.clear();
        s.has_data = false;
        s.art_data_url = None;
        s.progress = None;
    }
    let _ = app.emit("nowplaying://session", json!({ "active": true }));
    emit_meta(app, String::new(), String::new(), String::new(), false);
    let _ = app.emit("nowplaying://art", json!({ "dataUrl": serde_json::Value::Null }));
}

/// Any -> Inactive. Clears track state; emits session{false} only if we were
/// actually active (mirror-only sessions never emit a spurious end).
/// Caller must hold TRANSITION.
fn deactivate_inner(app: &AppHandle) {
    let was_active = {
        let mut s = SESSION.lock().unwrap();
        let was = s.active;
        s.active = false;
        s.title.clear();
        s.artist.clear();
        s.album.clear();
        s.has_data = false;
        s.art_data_url = None;
        s.progress = None;
        was
    };
    *EARLY_ART.lock().unwrap() = None;
    // Bump gen so any pending quiescence timer disarms itself.
    GEN.fetch_add(1, Ordering::SeqCst);
    QUIESCE_GEN.store(0, Ordering::SeqCst);
    if was_active {
        let _ = app.emit("nowplaying://session", json!({ "active": false }));
        crate::discovery::note_client_disconnected();
        crate::discovery::emit_devices();
    }
}

// ── file handlers ──────────────────────────────────────────────────────────

/// Parse a debounced -md rewrite and emit meta — only while Active (mirror
/// sessions rewrite this file too).
fn handle_md(app: &AppHandle, bytes: &[u8]) {
    if !SESSION.lock().unwrap().active {
        return;
    }
    let key = hash_bytes(bytes);
    if MD_HASH.swap(key, Ordering::SeqCst) == key {
        return; // unchanged content (dedupe torn re-reads / identical rewrites)
    }
    let (title, artist, album, has_data) = parse_md(bytes);
    emit_meta(app, title, artist, album, has_data);
    // If cover art for this same track arrived via `reread_art` just before
    // the poller saw `-md`, replay it once so `emit_meta` / `setNpMeta`
    // clearing the art doesn't leave the track without its cover.
    let early = EARLY_ART
        .lock()
        .unwrap()
        .take()
        .filter(|(at, _)| at.elapsed() < Duration::from_millis(1200));
    if let Some((_, art_bytes)) = early {
        CA_HASH.store(ca_key(&art_bytes), Ordering::SeqCst);
        emit_art(app, &art_bytes);
    } else if let Ok(ca_bytes) = std::fs::read(engines::ca_path()) {
        if ca_bytes.len() != 95 && sniff_image(&ca_bytes).is_some() {
            CA_HASH.store(ca_key(&ca_bytes), Ordering::SeqCst);
            emit_art(app, &ca_bytes);
        }
    }
}

/// Parse a debounced -ca rewrite and emit art — only while Active.
fn handle_ca(app: &AppHandle, bytes: &[u8]) {
    if !SESSION.lock().unwrap().active {
        return;
    }
    let key = ca_key(bytes);
    if CA_HASH.swap(key, Ordering::SeqCst) == key {
        return;
    }
    *EARLY_ART.lock().unwrap() = None;
    emit_art(app, bytes);
}

/// Immediate art re-read triggered by the 'coverart size .. written to' line.
fn reread_art(app: &AppHandle) {
    if !SESSION.lock().unwrap().active {
        return;
    }
    // Check -md first if it was already written on disk so `nowplaying://meta`
    // lands before `nowplaying://art` when both files arrive together.
    if let Ok(md_bytes) = std::fs::read(engines::md_path()) {
        handle_md(app, &md_bytes);
    }
    let bytes = match std::fs::read(engines::ca_path()) {
        Ok(b) => b,
        Err(_) => return,
    };
    let key = ca_key(&bytes);
    if CA_HASH.swap(key, Ordering::SeqCst) == key {
        return;
    }
    if bytes.len() != 95 && sniff_image(&bytes).is_some() {
        *EARLY_ART.lock().unwrap() = Some((Instant::now(), bytes.clone()));
    }
    emit_art(app, &bytes);
}

// ── emitters ───────────────────────────────────────────────────────────────

fn emit_meta(app: &AppHandle, title: String, artist: String, album: String, has_data: bool) {
    // Reset CA_HASH to placeholder whenever metadata changes: because both
    // `emit_meta` and the frontend's `setNpMeta` handle cover art, a consecutive
    // track on the SAME album (identical cover-art bytes) would otherwise be
    // suppressed by `CA_HASH.swap(key) == key`.
    CA_HASH.store(PLACEHOLDER_KEY, Ordering::SeqCst);
    let rev = {
        let mut s = SESSION.lock().unwrap();
        s.title = title.clone();
        s.artist = artist.clone();
        s.album = album.clone();
        s.has_data = has_data;
        s.rev += 1;
        s.rev
    };
    let _ = app.emit(
        "nowplaying://meta",
        json!({
            "title": title,
            "artist": artist,
            "album": album,
            "hasData": has_data,
            "rev": rev,
        }),
    );
}

fn emit_art(app: &AppHandle, bytes: &[u8]) {
    let data_url: Option<String> = if bytes.len() == 95 {
        None // placeholder file
    } else if let Some(mime) = sniff_image(bytes) {
        let b64 = base64_encode(bytes);
        if b64.len() > 4 * 1024 * 1024 {
            return; // absurdly large art — skip the emit, keep the prior image
        }
        Some(format!("data:{};base64,{}", mime, b64))
    } else {
        None // torn / unknown bytes — treat as no art; the next poll re-reads
    };
    SESSION.lock().unwrap().art_data_url = data_url.clone();
    let _ = app.emit("nowplaying://art", json!({ "dataUrl": data_url }));
}

// ── parsing helpers ────────────────────────────────────────────────────────

/// Parse the -md snapshot into (title, artist, album, has_data). Strips the
/// trailing NUL, treats "no data" as cleared, falls back to 'Album artist'.
fn parse_md(bytes: &[u8]) -> (String, String, String, bool) {
    // strip trailing NUL(s) from write_metadata's strlen+1 write
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    let text = String::from_utf8_lossy(&bytes[..end]);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("no data") {
        return (String::new(), String::new(), String::new(), false);
    }

    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut album_artist = String::new();
    for raw in text.lines() {
        let raw = raw.trim();
        if let Some((label, value)) = raw.split_once(':') {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            // Labels are case-sensitive (uxplay emits fixed English labels).
            match label.trim() {
                "Title" => title = value.to_string(),
                "Artist" => artist = value.to_string(),
                "Album" => album = value.to_string(),
                "Album artist" => album_artist = value.to_string(),
                _ => {}
            }
        }
    }
    if artist.is_empty() && !album_artist.is_empty() {
        artist = album_artist;
    }
    let has_data = !title.is_empty() || !artist.is_empty() || !album.is_empty();
    (title, artist, album, has_data)
}

/// Parse "audio progress (min:sec): M:SS; remaining: M:SS; track length M:SS"
/// into (position_secs, duration_secs). Split on ';' then pull the trailing
/// M:SS out of each field — no regex crate in this project.
fn parse_progress(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.split(';');
    let position = parse_mmss(parts.next()?)?;
    let _remaining = parts.next()?; // recomputed as duration - position
    let duration = parse_mmss(parts.next()?)?;
    Some((position, duration))
}

/// Extract the trailing "M:SS" from a field (ignoring any label prefix) and
/// return whole seconds. Minutes are unpadded and may exceed 59.
fn parse_mmss(field: &str) -> Option<u64> {
    let mut it = field.rsplit(':');
    let sec: u64 = it.next()?.trim().parse().ok()?;
    // The minute value is the last whitespace-token before the final colon
    // (handles both "...: 1:05" and "track length 3:42").
    let min: u64 = it.next()?.split_whitespace().last()?.parse().ok()?;
    Some(min * 60 + sec)
}

/// Sniff the image type by magic bytes — never trust the .img extension.
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        Some("image/jpeg")
    } else if bytes.len() >= 4 && bytes[0] == 0x89 && bytes[1] == 0x50 && bytes[2] == 0x4E && bytes[3] == 0x47 {
        Some("image/png")
    } else {
        None
    }
}

/// Dedupe key for -ca: the 95-byte placeholder collapses to a fixed sentinel
/// so it dedupes against the activate() seed.
fn ca_key(bytes: &[u8]) -> u64 {
    if bytes.len() == 95 {
        PLACEHOLDER_KEY
    } else {
        hash_bytes(bytes)
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn file_sig(p: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some((mtime, m.len()))
}

/// Standard RFC 4648 base64 (no line breaks). Hand-rolled to avoid pulling in
/// a crate for a single encode of cover-art bytes.
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn airplay2_session_started(app: &AppHandle) {
    activate(app);
}

pub fn airplay2_metadata(
    app: &AppHandle,
    title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
) {
    emit_meta(
        app,
        title.unwrap_or("").to_string(),
        artist.unwrap_or("").to_string(),
        album.unwrap_or("").to_string(),
        title.is_some() || artist.is_some() || album.is_some(),
    );
}

pub fn airplay2_artwork(app: &AppHandle, data_url: Option<String>) {
    let mut s = SESSION.lock().unwrap();
    s.art_data_url = data_url.clone();
    if s.active {
        let _ = app.emit("nowplaying://art", serde_json::json!({ "dataUrl": data_url }));
    }
}

pub fn airplay2_paused(app: &AppHandle, paused: bool) {
    let _ = app.emit("nowplaying://paused", serde_json::json!({ "paused": paused }));
}

pub fn airplay2_progress(app: &AppHandle, pos: u64, dur: u64) {
    let active = {
        let mut s = SESSION.lock().unwrap();
        if s.active {
            s.progress = Some((pos, dur, Instant::now()));
        }
        s.active
    };
    if active {
        let remaining = dur.saturating_sub(pos);
        let _ = app.emit(
            "nowplaying://progress",
            serde_json::json!({ "position": pos, "duration": dur, "remaining": remaining }),
        );
    }
}

pub fn airplay2_session_ended(app: &AppHandle) {
    deactivate(app);
}

