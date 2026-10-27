//! Scrcpy sidecar integration for 60fps wireless Android screen mirroring docked into Mirr's glass UI.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static SCRCPY_CHILD: Mutex<Option<(Child, u32)>> = Mutex::new(None);

#[derive(Clone, serde::Serialize, Debug)]
pub struct AdbDevice {
    pub serial: String,
    pub model: String,
    pub state: String,
    pub is_wireless: bool,
}

fn find_in_path(bin_name: &str) -> Option<PathBuf> {
    if let Some(paths) = std::env::var_os("PATH") {
        for p in std::env::split_paths(&paths) {
            let candidate = p.join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Locate `scrcpy.exe` on system or in bundled engines directory.
pub fn find_scrcpy(app: Option<&AppHandle>) -> Option<PathBuf> {
    // 1. Check bundled resources
    if let Some(h) = app {
        if let Ok(res) = h.path().resource_dir() {
            let p1 = res.join("engines").join("scrcpy.exe");
            if p1.is_file() {
                return Some(p1);
            }
            let p2 = res.join("engines").join("scrcpy").join("scrcpy.exe");
            if p2.is_file() {
                return Some(p2);
            }
        }
    }

    // 2. Next to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("engines").join("scrcpy.exe");
            if p.is_file() {
                return Some(p);
            }
            let p2 = parent.join("scrcpy.exe");
            if p2.is_file() {
                return Some(p2);
            }
        }
    }

    // 3. Current working directory
    let cwd_scrcpy = PathBuf::from("engines").join("scrcpy.exe");
    if cwd_scrcpy.is_file() {
        return Some(cwd_scrcpy);
    }

    // 4. Standard Windows package locations (winget, scoop, choco, Program Files)
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let winget_scrcpy = PathBuf::from(&local_app_data)
            .join("Programs")
            .join("scrcpy")
            .join("scrcpy.exe");
        if winget_scrcpy.is_file() {
            return Some(winget_scrcpy);
        }
    }

    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let scoop_scrcpy = PathBuf::from(user_profile)
            .join("scoop")
            .join("apps")
            .join("scrcpy")
            .join("current")
            .join("scrcpy.exe");
        if scoop_scrcpy.is_file() {
            return Some(scoop_scrcpy);
        }
    }

    let prog_files = PathBuf::from(r"C:\Program Files\scrcpy\scrcpy.exe");
    if prog_files.is_file() {
        return Some(prog_files);
    }

    let choco_scrcpy = PathBuf::from(r"C:\ProgramData\chocolatey\bin\scrcpy.exe");
    if choco_scrcpy.is_file() {
        return Some(choco_scrcpy);
    }

    // 5. PATH search
    find_in_path("scrcpy.exe")
}

/// Locate `adb.exe` on system or in bundled engines directory.
pub fn find_adb(app: Option<&AppHandle>) -> Option<PathBuf> {
    if let Some(h) = app {
        if let Ok(res) = h.path().resource_dir() {
            let p1 = res.join("engines").join("adb.exe");
            if p1.is_file() {
                return Some(p1);
            }
            let p2 = res.join("engines").join("scrcpy").join("adb.exe");
            if p2.is_file() {
                return Some(p2);
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("engines").join("adb.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }

    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let sdk_adb = PathBuf::from(&local_app_data)
            .join("Android")
            .join("Sdk")
            .join("platform-tools")
            .join("adb.exe");
        if sdk_adb.is_file() {
            return Some(sdk_adb);
        }

        let scrcpy_adb = PathBuf::from(&local_app_data)
            .join("Programs")
            .join("scrcpy")
            .join("adb.exe");
        if scrcpy_adb.is_file() {
            return Some(scrcpy_adb);
        }
    }

    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let scoop_adb = PathBuf::from(user_profile)
            .join("scoop")
            .join("apps")
            .join("scrcpy")
            .join("current")
            .join("adb.exe");
        if scoop_adb.is_file() {
            return Some(scoop_adb);
        }
    }

    let prog_adb = PathBuf::from(r"C:\Program Files\scrcpy\adb.exe");
    if prog_adb.is_file() {
        return Some(prog_adb);
    }

    find_in_path("adb.exe")
}

/// Query ADB for connected devices (USB and Wi-Fi).
pub fn list_adb_devices(app: Option<&AppHandle>) -> Vec<AdbDevice> {
    let adb = match find_adb(app) {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut cmd = Command::new(adb);
    cmd.args(["devices", "-l"]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = match cmd.output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return Vec::new(),
    };

    let mut list = Vec::new();
    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let serial = parts[0].to_string();
            let state = parts[1].to_string();
            let is_wireless = serial.contains(':');

            let mut model = "Android Device".to_string();
            for part in &parts[2..] {
                if let Some(m) = part.strip_prefix("model:") {
                    model = m.replace('_', " ");
                }
            }

            list.push(AdbDevice {
                serial,
                model,
                state,
                is_wireless,
            });
        }
    }

    list
}

/// Connect wirelessly to an Android device via `adb connect <ip>:5555`.
pub fn connect_wireless_adb(app: Option<&AppHandle>, ip: &str) -> anyhow::Result<String> {
    let adb = find_adb(app).ok_or_else(|| anyhow::anyhow!("adb.exe not found on system"))?;
    let target = if ip.contains(':') {
        ip.to_string()
    } else {
        format!("{ip}:5555")
    };

    let mut cmd = Command::new(adb);
    cmd.args(["connect", &target]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run adb connect: {e}"))?;
    let res = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || res.starts_with("cannot connect") || res.contains("failed to connect") {
        return Err(anyhow::anyhow!("ADB connection error: {res}"));
    }
    Ok(res)
}

/// Launch Scrcpy sidecar and dock its window into Mirr's UI stage.
pub fn start_scrcpy(app: &AppHandle, target_ip_or_serial: Option<String>) -> anyhow::Result<()> {
    stop_scrcpy();

    let scrcpy_path = find_scrcpy(Some(app))
        .ok_or_else(|| anyhow::anyhow!("Scrcpy engine (scrcpy.exe) is not found"))?;

    // Determine target serial: if wireless IP, connect via ADB first and format target as IP:PORT
    let mut serial_target = None;
    if let Some(ref target) = target_ip_or_serial {
        let trimmed = target.trim();
        if !trimmed.is_empty() {
            if trimmed.contains('.') {
                let target_with_port = if trimmed.contains(':') {
                    trimmed.to_string()
                } else {
                    format!("{trimmed}:5555")
                };
                connect_wireless_adb(Some(app), &target_with_port)?;
                serial_target = Some(target_with_port);
            } else {
                serial_target = Some(trimmed.to_string());
            }
        }
    }

    let mut cmd = Command::new(scrcpy_path);
    cmd.args([
        "--window-title=Mirr-Scrcpy",
        "--max-fps=60",
        "--stay-awake",
        "--video-codec=h264",
        "--no-audio",
    ]);

    if let Some(ref s) = serial_target {
        cmd.args(["--serial", s]);
    }

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to start Scrcpy engine: {e}"))?;

    let pid = child.id();

    // Attach to Job Object and wake up the window docking watcher
    #[cfg(windows)]
    {
        crate::engines::job::assign(&child);
        crate::embed::ensure_watcher(app.clone());
        crate::embed::set_engine_pid(pid);
        crate::embed::set_seeking(true);
    }

    let _ = app.emit(
        "mirror://started",
        serde_json::json!({
            "engine": "scrcpy",
            "pid": pid,
            "target": serial_target
        }),
    );

    let mut lock = SCRCPY_CHILD.lock().unwrap();
    *lock = Some((child, pid));

    // Monitor Scrcpy child process so UI state and embed PID reset immediately on termination
    let app_watcher = app.clone();
    std::thread::Builder::new()
        .name("scrcpy-supervisor".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                let mut lock = SCRCPY_CHILD.lock().unwrap();
                if let Some((ref mut child, child_pid)) = *lock {
                    if child_pid != pid {
                        break;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            eprintln!("[scrcpy] Process {pid} terminated: {status}");
                            *lock = None;
                            #[cfg(windows)]
                            if crate::embed::engine_pid() == pid {
                                crate::embed::set_engine_pid(0);
                            }
                            let _ = app_watcher.emit("mirror://ended", ());
                            break;
                        }
                        Ok(None) => {} // still active
                        Err(e) => {
                            eprintln!("[scrcpy] Supervisor error: {e}");
                            *lock = None;
                            #[cfg(windows)]
                            if crate::embed::engine_pid() == pid {
                                crate::embed::set_engine_pid(0);
                            }
                            let _ = app_watcher.emit("mirror://ended", ());
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
        })
        .ok();

    Ok(())
}

/// Stop the running Scrcpy process.
pub fn stop_scrcpy() {
    let taken = {
        let mut lock = SCRCPY_CHILD.lock().unwrap();
        lock.take()
    };

    if let Some((mut child, _)) = taken {
        let _ = child.kill();
        let _ = child.wait();
        #[cfg(windows)]
        crate::embed::set_engine_pid(0);
    }
}

pub fn is_scrcpy_running() -> bool {
    let mut lock = SCRCPY_CHILD.lock().unwrap();
    if let Some((child, _)) = lock.as_mut() {
        match child.try_wait() {
            Ok(None) => true,
            _ => {
                *lock = None;
                false
            }
        }
    } else {
        false
    }
}
