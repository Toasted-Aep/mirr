//! Sidecar engine supervisor.
//!
//! The Apple protocol engines are mature native programs that we run as separate,
//! supervised child processes rather than linking in-process. This keeps a clean
//! licensing boundary (UxPlay/GStreamer are GPL) and isolates crashes.
//!
//!   * AirPlay screen + audio RECEIVE  -> UxPlay (GStreamer)         `engines/uxplay.exe`
//!   * AirPlay 2 audio RECEIVE         -> shairport-sync (bundled)   `engines/shairport.exe`  [stretch]
//!   * AirCast (PC -> speakers) SEND   -> pyatv helper               `engines/aircast.exe`
//!
//! UxPlay is resolved from the bundled `engines/` resource dir first, then from
//! the standard winget install (`leapbtw.uxplay`). It is spawned with a hidden
//! console (CREATE_NO_WINDOW) inside a kill-on-close Job Object (no orphans if
//! Mirr dies), its output is parsed for connection state, and its video window
//! is docked into the main UI by `embed`.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Win32: don't create a console window for the child process.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Default install location of the winget UxPlay package (`leapbtw.uxplay`).
const UXPLAY_INSTALL: &str = r"C:\Program Files (x86)\uxplay-windows\_internal";

#[derive(Default)]
struct Supervised {
    /// (child, generation) — the generation tag lets a stale log-reader thread
    /// recognise that the engine it watched has been replaced (PIDs get reused
    /// on Windows, so comparing raw PIDs is not enough).
    airplay_rx: Option<(Child, u64)>,
    airplay2_rx: Option<Child>,
    aircast: Option<Child>,
}

static STATE: Mutex<Supervised> = Mutex::new(Supervised {
    airplay_rx: None,
    airplay2_rx: None,
    aircast: None,
});
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Last (state, detail) emitted — lets the frontend query the current state
/// after it attaches its listeners (early events would otherwise be lost).
static LAST_STATE: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

/// Kill-on-close Job Object shared by every engine child: if Mirr dies for any
/// reason (including a crash), the OS reaps the engines with it.
#[cfg(windows)]
mod job {
    use once_cell::sync::Lazy;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    static JOB: Lazy<Option<Job>> = Lazy::new(|| unsafe {
        let handle = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&info) as u32,
        )
        .ok()?;
        Some(Job(handle))
    });

    pub fn assign(child: &std::process::Child) {
        use std::os::windows::io::AsRawHandle;
        if let Some(job) = JOB.as_ref() {
            unsafe {
                let _ = AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle() as _));
            }
        }
    }
}

fn engine_path(app: &AppHandle, exe: &str) -> anyhow::Result<PathBuf> {
    let dir = app
        .path()
        .resource_dir()
        .map_err(|e| anyhow::anyhow!("resource_dir: {e}"))?;
    Ok(dir.join("engines").join(exe))
}

/// Locate uxplay.exe plus the bin dir (DLLs) and GStreamer plugin dir it needs.
fn resolve_uxplay(app: &AppHandle) -> anyhow::Result<(PathBuf, PathBuf, Option<PathBuf>)> {
    // 1) bundled next to the app resources
    if let Ok(bundled) = engine_path(app, "uxplay.exe") {
        if bundled.is_file() {
            let bin = bundled.parent().unwrap().to_path_buf();
            let plugins = bin.join("gstreamer-1.0");
            return Ok((bundled, bin.clone(), plugins.is_dir().then_some(plugins)));
        }
    }
    // 2) winget install
    let base = PathBuf::from(UXPLAY_INSTALL);
    let exe = base.join("bin").join("uxplay.exe");
    if exe.is_file() {
        let bin = base.join("bin");
        let lib = base.join("lib").join("gstreamer-1.0");
        let plugins = if lib.is_dir() {
            Some(lib)
        } else {
            find_dir_named(&base, "gstreamer-1.0", 3)
        };
        return Ok((exe, bin, plugins));
    }
    // 3) PATH
    if let Ok(found) = which("uxplay.exe") {
        let bin = found.parent().unwrap().to_path_buf();
        return Ok((found, bin, None));
    }
    Err(anyhow::anyhow!(
        "UxPlay not found. Install it with:  winget install --id leapbtw.uxplay -e"
    ))
}

fn which(exe: &str) -> anyhow::Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(anyhow::anyhow!("{exe} not on PATH"))
}

fn find_dir_named(root: &std::path::Path, name: &str, depth: u32) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == name) {
                return Some(p);
            }
            subdirs.push(p);
        }
    }
    subdirs
        .iter()
        .find_map(|d| find_dir_named(d, name, depth - 1))
}

fn emit_status(app: &AppHandle, state: &str, detail: &str) {
    *LAST_STATE.lock().unwrap() = (state.to_string(), detail.to_string());
    let _ = app.emit("airplay://status", json!({ "state": state, "detail": detail }));
}

pub fn receiver_state() -> serde_json::Value {
    let (state, detail) = LAST_STATE.lock().unwrap().clone();
    json!({ "state": state, "detail": detail })
}

/// Start the AirPlay screen/audio receiver. Advertises this PC as "Mirr".
/// The engine runs with a hidden console; its video output is docked into the
/// main window by `embed::ensure_watcher`.
pub fn start_airplay_receiver(app: &AppHandle) -> anyhow::Result<()> {
    let mut st = STATE.lock().unwrap();
    if let Some((child, _)) = st.airplay_rx.as_mut() {
        if child.try_wait()?.is_none() {
            return Ok(()); // already running
        }
        st.airplay_rx = None; // reap a dead child, then restart
    }

    let (exe, bin, plugins) = resolve_uxplay(app)?;
    let name = "Mirr"; // the name iPhones see in the Screen Mirroring sheet

    let mut cmd = Command::new(&exe);
    cmd.args([
        "-n", name,
        "-nh",       // no "@hostname" suffix
        "-vsync", "no", // favour latency over AV-sync in mirror mode (sink runs sync=false)
        "-fps", "60",
        "-nofreeze", // don't leave a frozen last frame after a connection reset
    ])
    .current_dir(&bin)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    // Full-GPU pipeline (decode → convert → present without sysmem copies).
    // Only when the d3d11 plugin is actually present; otherwise UxPlay's
    // decodebin default still works, just slower.
    let has_d3d11 = plugins
        .as_ref()
        .is_some_and(|p| p.join("libgstd3d11.dll").is_file());
    if has_d3d11 {
        cmd.args([
            "-vd", "d3d11h264dec",
            "-vc", "d3d11convert",
            "-vs", "d3d11videosink",
        ]);
    }

    // GStreamer needs its plugin dir and the bundled DLLs on PATH.
    if let Some(plugins) = &plugins {
        cmd.env("GST_PLUGIN_PATH", plugins)
            .env("GST_PLUGIN_SYSTEM_PATH", plugins);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("{};{}", bin.display(), path));

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start AirPlay engine ({}): {e}", exe.display()))?;

    #[cfg(windows)]
    {
        job::assign(&child);
        crate::embed::set_engine_pid(child.id());
        crate::embed::ensure_watcher(app.clone());
    }
    emit_status(app, "starting", name);

    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    // Parse engine output for lifecycle events; EOF means the engine exited.
    if let Some(stdout) = child.stdout.take() {
        spawn_log_reader(app.clone(), generation, stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_reader(app.clone(), generation, stderr);
    }

    st.airplay_rx = Some((child, generation));
    Ok(())
}

/// Markers UxPlay prints when a mirroring session ends. The window watcher is
/// the primary end-of-session signal; these catch the "frozen last frame"
/// cases where UxPlay keeps the window alive.
fn is_stream_stop_marker(line: &str) -> bool {
    line.contains("teardown")
        || line.contains("connection closed")
        || line.contains("connection reset")
        || line.contains("client silence")
        || line.contains("stopped mirroring")
        || line.contains("video renderer reset")
}

fn spawn_log_reader(app: AppHandle, generation: u64, pipe: impl std::io::Read + Send + 'static) {
    std::thread::spawn(move || {
        // Read raw bytes: GStreamer happily logs non-UTF-8 (localized device
        // names), which would abort a .lines() loop long before EOF.
        let mut reader = std::io::BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break, // EOF or unreadable pipe
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end();
            let l = line.to_lowercase();
            if l.contains("initialized server socket") {
                emit_status(&app, "ready", "");
            } else if l.contains("connection request") {
                emit_status(&app, "connecting", "");
            }
            if is_stream_stop_marker(&l) {
                crate::embed::note_stream_stop(app.clone());
            }
            let _ = app.emit("airplay://log", line.to_string());
        }
        // EOF — the engine died or was stopped. Take the child under the lock
        // only if it is still our generation, but wait() OUTSIDE the lock so a
        // slow reap can never deadlock the supervisor.
        let stale_child = {
            let mut st = STATE.lock().unwrap();
            match &st.airplay_rx {
                Some((_, gen)) if *gen == generation => {
                    st.airplay_rx.take().map(|(child, _)| child)
                }
                _ => None,
            }
        };
        if let Some(mut child) = stale_child {
            #[cfg(windows)]
            crate::embed::set_engine_pid(0);
            let _ = child.wait();
            emit_status(&app, "stopped", "");
        }
    });
}

pub fn stop_airplay_receiver(app: &AppHandle) {
    let taken = {
        let mut st = STATE.lock().unwrap();
        let taken = st.airplay_rx.take();
        if taken.is_some() {
            // clear the pid while still holding the lock so a concurrent
            // start can't have its fresh pid clobbered
            #[cfg(windows)]
            crate::embed::set_engine_pid(0);
        }
        taken
    };
    if let Some((mut child, _)) = taken {
        let _ = child.kill();
        let _ = child.wait();
        emit_status(app, "stopped", "");
    }
}

/// Start AirCast: capture this PC's audio (WASAPI loopback) and stream to `target_ip`.
pub fn start_aircast(app: &AppHandle, target_ip: &str) -> anyhow::Result<()> {
    let mut st = STATE.lock().unwrap();
    // retarget: never leak a second concurrent capture process
    if let Some(mut old) = st.aircast.take() {
        let _ = old.kill();
        let _ = old.wait();
    }
    let path = engine_path(app, "aircast.exe")?;
    let mut cmd = Command::new(path);
    cmd.args(["--target", target_ip, "--source", "wasapi-loopback"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start AirCast engine: {e}"))?;
    #[cfg(windows)]
    job::assign(&child);
    st.aircast = Some(child);
    Ok(())
}

#[allow(dead_code)]
pub fn stop_aircast() {
    if let Some(mut c) = STATE.lock().unwrap().aircast.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Start the AirPlay 2 lossless audio receiver (stretch goal on Windows).
pub fn start_airplay2_receiver(app: &AppHandle) -> anyhow::Result<()> {
    let mut st = STATE.lock().unwrap();
    if let Some(child) = st.airplay2_rx.as_mut() {
        if child.try_wait()?.is_none() {
            return Ok(()); // already running
        }
        st.airplay2_rx = None; // reap a dead child, then restart
    }
    let path = engine_path(app, "shairport.exe")?;
    let name = crate::localsend::device_alias();
    let mut cmd = Command::new(path);
    cmd.args(["-a", &name]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("AirPlay 2 engine unavailable on this platform yet: {e}"))?;
    #[cfg(windows)]
    job::assign(&child);
    st.airplay2_rx = Some(child);
    Ok(())
}

/// Kill every engine — call on app exit. The Job Object is the crash-path
/// backstop; this is the orderly path.
pub fn shutdown_all() {
    let mut st = STATE.lock().unwrap();
    if let Some((c, _)) = st.airplay_rx.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
    st.airplay_rx = None;
    let st = &mut *st;
    for child in [&mut st.airplay2_rx, &mut st.aircast] {
        if let Some(c) = child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        *child = None;
    }
}
