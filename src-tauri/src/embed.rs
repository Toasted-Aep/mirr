//! Docks the UxPlay/GStreamer video window inside the main Mirr window.
//!
//! UxPlay renders incoming mirror video into its own top-level window
//! ("Direct3D11 renderer"). Instead of letting it float as a separate window,
//! a watcher thread finds the engine's video window, strips its decorations,
//! reparents it into the Mirr window as a WS_CHILD and keeps it glued to the
//! rectangle the frontend reports for its video stage (physical pixels, client
//! coordinates). A rounded GDI region keeps the video corners in line with the
//! glass UI and doubles as the crop window for digital zoom.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindow, IsWindowVisible, MoveWindow, PostMessageW, SetParent, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, HWND_TOP, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WM_CLOSE, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct DockRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    radius: i32,
    /// digital zoom in percent (100 = fit)
    zoom: i32,
}

/// PID of the running engine process (0 = none).
static ENGINE_PID: AtomicU32 = AtomicU32::new(0);
/// HWND of the embedded video window (0 = not embedded).
static VIDEO_HWND: AtomicIsize = AtomicIsize::new(0);
/// A window SetParent failed on — never retried, so we don't corrupt it in a loop.
static FAILED_HWND: AtomicIsize = AtomicIsize::new(0);
/// Last window we emitted mirror://detected for (throttles the event).
static DETECTED_HWND: AtomicIsize = AtomicIsize::new(0);
/// Whether the docked video should be shown (receive view active) or hidden.
static VISIBLE: AtomicBool = AtomicBool::new(true);
/// Where the frontend wants the video (physical px, main-window client coords).
static TARGET: Mutex<Option<DockRect>> = Mutex::new(None);
/// Last rect actually applied to the child, to avoid redundant Win32 calls.
static APPLIED: Mutex<Option<DockRect>> = Mutex::new(None);
static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
/// Remaining fast watcher ticks: >0 means a mirror handshake is plausibly in
/// flight, so keep sweeping for the video window 4x/s. Burned down one per tick.
static SEEK_TICKS: AtomicU32 = AtomicU32::new(0);
/// A `note_stream_stop` grace-period thread is already running.
static STOP_PENDING: AtomicBool = AtomicBool::new(false);

/// How long (in fast ticks) one `connection request` keeps the watcher hot.
/// 60 * 250 ms = 15 s, comfortably longer than the handshake-to-first-frame gap
/// even on a slow network.
const SEEK_FAST_TICKS: u32 = 60;

pub fn set_engine_pid(pid: u32) {
    ENGINE_PID.store(pid, Ordering::SeqCst);
}

pub fn mirror_active() -> bool {
    VIDEO_HWND.load(Ordering::SeqCst) != 0
}

/// The engine's log reader saw a connection request (or wants to cancel one):
/// put the watcher on its fast cadence so a device's video window is docked
/// within a fraction of a second of appearing.
///
/// This is only an accelerator, never a gate — the watcher still sweeps every
/// second when it is false, so docking keeps working if UxPlay ever reworks the
/// wording of that log line.
pub fn set_seeking(seeking: bool) {
    SEEK_TICKS.store(if seeking { SEEK_FAST_TICKS } else { 0 }, Ordering::SeqCst);
}

/// Frontend reports the stage rectangle; applied immediately when docked.
pub fn set_target_rect(x: i32, y: i32, w: i32, h: i32, radius: i32, zoom: i32) {
    let rect = DockRect { x, y, w, h, radius, zoom };
    *TARGET.lock().unwrap() = Some(rect);
    let hwnd = VIDEO_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        apply_rect(HWND(hwnd as _), rect);
    }
}

/// Show/hide the docked video (the native child would otherwise paint over
/// every view; the frontend calls this when the user switches panes).
pub fn set_video_visible(visible: bool) {
    VISIBLE.store(visible, Ordering::SeqCst);
    let hwnd = VIDEO_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = ShowWindow(HWND(hwnd as _), if visible { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

/// The engine logged a teardown/close marker. UxPlay sometimes leaves a frozen
/// last frame in place instead of closing its window — give it a grace period,
/// then close (or as a last resort hide) the window ourselves so the UI can
/// return to the idle state.
pub fn note_stream_stop(app: AppHandle) {
    // One teardown prints several matching markers in a row ("teardown",
    // "connection closed", "video renderer reset", ...), and both pipes are
    // read by their own thread — unguarded, each line started its own ~2 s
    // thread and they raced to close/hide the same HWND. At most one in flight.
    if STOP_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        stop_after_grace(app);
        // Single exit point: every early return inside stop_after_grace lands
        // here, so the guard can never stick and swallow a later teardown.
        STOP_PENDING.store(false, Ordering::SeqCst);
    });
}

fn stop_after_grace(app: AppHandle) {
    std::thread::sleep(Duration::from_millis(1200));
    let hwnd = VIDEO_HWND.load(Ordering::SeqCst);
    if hwnd == 0 {
        return; // already gone — the watcher handled it
    }
    unsafe {
        let h = HWND(hwnd as _);
        if IsWindow(Some(h)).as_bool() {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    std::thread::sleep(Duration::from_millis(800));
    if VIDEO_HWND.load(Ordering::SeqCst) != hwnd {
        return; // watcher noticed the close in the meantime
    }
    unsafe {
        let h = HWND(hwnd as _);
        if IsWindow(Some(h)).as_bool() {
            let _ = ShowWindow(h, SW_HIDE); // frozen-frame window refuses to die
        }
    }
    VIDEO_HWND.store(0, Ordering::SeqCst);
    *APPLIED.lock().unwrap() = None;
    let _ = app.emit("mirror://ended", ());
}

/// Spawn the background watcher (idempotent). It embeds the engine's video
/// window when one appears and reports when the mirror session ends.
pub fn ensure_watcher(app: AppHandle) {
    if WATCHER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || loop {
        // Cadence is state-dependent. The undocked branch below is a full
        // EnumWindows sweep of every top-level window on the desktop, and
        // "open, nothing connected" is this app's steady state — polling that
        // 4x/s forever is pure waste. Stay fast only when it buys something:
        //   * docked      — the child rect must stay glued to the stage while
        //                   the user drags/resizes the window;
        //   * seeking     — a handshake is in flight (engines.rs saw a
        //                   `connection request`), so the video window is about
        //                   to appear and must dock immediately.
        // Otherwise 1 s, which still bounds worst-case docking latency if the
        // log marker is ever missed. Do NOT stop polling entirely: the marker
        // is an accelerator, not the source of truth.
        let fast = VIDEO_HWND.load(Ordering::SeqCst) != 0 || SEEK_TICKS.load(Ordering::SeqCst) > 0;
        std::thread::sleep(Duration::from_millis(if fast { 250 } else { 1000 }));
        let _ = SEEK_TICKS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |t| {
            if t > 0 {
                Some(t - 1)
            } else {
                None // already idle — leave it alone
            }
        });

        let embedded = VIDEO_HWND.load(Ordering::SeqCst);
        if embedded != 0 {
            let h = HWND(embedded as _);
            if !unsafe { IsWindow(Some(h)).as_bool() } {
                VIDEO_HWND.store(0, Ordering::SeqCst);
                *APPLIED.lock().unwrap() = None;
                let _ = app.emit("mirror://ended", ());
            } else if let Some(rect) = *TARGET.lock().unwrap() {
                // re-assert the latest rect; apply_rect dedupes via APPLIED,
                // so this only costs Win32 calls when something changed
                apply_rect(h, rect);
            }
            continue;
        }

        let pid = ENGINE_PID.load(Ordering::SeqCst);
        if pid == 0 {
            continue;
        }
        let Some(child) = find_video_window(pid) else {
            continue;
        };
        let target = *TARGET.lock().unwrap();
        match target {
            Some(rect) => {
                if embed(&app, child, rect).is_ok() {
                    VIDEO_HWND.store(child.0 as isize, Ordering::SeqCst);
                    let _ = app.emit("mirror://started", ());
                }
            }
            None => {
                // No dockable rect yet (e.g. app deep-linked into another
                // view) — ask the frontend to surface the receive stage.
                if DETECTED_HWND.swap(child.0 as isize, Ordering::SeqCst) != child.0 as isize {
                    let _ = app.emit("mirror://detected", ());
                }
            }
        }
    });
}

/// Find a visible window owned by `pid` with a real client area, preferring
/// GStreamer's video-window class so an engine error dialog is never docked.
fn find_video_window(pid: u32) -> Option<HWND> {
    struct Ctx {
        pid: u32,
        skip: isize,
        gst: Option<HWND>,
        any: Option<HWND>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx) };
        if hwnd.0 as isize == ctx.skip {
            return BOOL(1);
        }
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        if owner != ctx.pid || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }
        let mut class = [0u16; 64];
        let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        let class = String::from_utf16_lossy(&class[..n]).to_ascii_uppercase();
        if class == "CONSOLEWINDOWCLASS" || class.contains("IME") {
            return BOOL(1);
        }
        let mut r = windows::Win32::Foundation::RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut r) }.is_ok()
            && (r.right - r.left) > 100
            && (r.bottom - r.top) > 100
        {
            if class.contains("GST") || class.contains("D3D") {
                ctx.gst = Some(hwnd);
                return BOOL(0); // best possible match — stop
            }
            if ctx.any.is_none() {
                ctx.any = Some(hwnd);
            }
        }
        BOOL(1)
    }
    let mut ctx = Ctx {
        pid,
        skip: FAILED_HWND.load(Ordering::SeqCst),
        gst: None,
        any: None,
    };
    unsafe {
        // EnumWindows returns Err when the callback stops early — not a failure here.
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.gst.or(ctx.any)
}

/// Reparent the engine window into the main window as a borderless child.
/// Restores the original styles if SetParent fails, and blacklists the window
/// so the watcher doesn't corrupt it again on the next tick.
fn embed(app: &AppHandle, child: HWND, rect: DockRect) -> anyhow::Result<()> {
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow::anyhow!("main window missing"))?;
    let parent = HWND(main.hwnd()?.0 as _);
    unsafe {
        let old_style = GetWindowLongPtrW(child, GWL_STYLE);
        let old_ex = GetWindowLongPtrW(child, GWL_EXSTYLE);
        let style = (WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0) as isize;
        SetWindowLongPtrW(child, GWL_STYLE, style);
        SetWindowLongPtrW(child, GWL_EXSTYLE, 0);
        if let Err(e) = SetParent(child, Some(parent)) {
            SetWindowLongPtrW(child, GWL_STYLE, old_style);
            SetWindowLongPtrW(child, GWL_EXSTYLE, old_ex);
            FAILED_HWND.store(child.0 as isize, Ordering::SeqCst);
            return Err(anyhow::anyhow!("SetParent failed: {e}"));
        }
        SetWindowPos(
            child,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )?;
        if !VISIBLE.load(Ordering::SeqCst) {
            let _ = ShowWindow(child, SW_HIDE);
        }
    }
    // Fresh window: forget whatever was applied to the previous session's
    // window so apply_rect can't dedupe against stale state.
    *APPLIED.lock().unwrap() = None;
    // A new session is live: drop any leftover teardown guard from the previous
    // one so this session's own stop markers are not swallowed.
    STOP_PENDING.store(false, Ordering::SeqCst);
    apply_rect(child, rect);
    Ok(())
}

fn apply_rect(child: HWND, rect: DockRect) {
    {
        let mut applied = APPLIED.lock().unwrap();
        if *applied == Some(rect) {
            return;
        }
        *applied = Some(rect);
    }
    // Digital zoom: oversize the window around the stage centre and clip the
    // region back to the stage, so the sink scales the video up for us.
    let z = f64::from(rect.zoom.clamp(100, 400)) / 100.0;
    let cw = (f64::from(rect.w) * z).round() as i32;
    let ch = (f64::from(rect.h) * z).round() as i32;
    let cx = rect.x - (cw - rect.w) / 2;
    let cy = rect.y - (ch - rect.h) / 2;
    let off_x = rect.x - cx;
    let off_y = rect.y - cy;
    unsafe {
        let _ = MoveWindow(child, cx, cy, cw, ch, true);
        if rect.radius == 0 && rect.zoom == 100 {
            // Square corners, no zoom crop (theater mode): the window IS its
            // own rectangle, so a region would only describe what the window
            // already is — while denying the child DWM's unclipped fast path.
            // None clears any region left by a previous rect. Do not "simplify"
            // this back into an unconditional CreateRoundRectRgn.
            SetWindowRgn(child, None, true);
        } else {
            // Rounded corners to match the glass stage; the system takes
            // ownership of the region handle after SetWindowRgn.
            let rgn = CreateRoundRectRgn(
                off_x,
                off_y,
                off_x + rect.w + 1,
                off_y + rect.h + 1,
                rect.radius * 2,
                rect.radius * 2,
            );
            SetWindowRgn(child, Some(rgn), true);
        }
    }
}
