//! AirConnect sidecar bridge integration (`airupnp` and `aircast` binaries).
//!
//! AirConnect bridges AirPlay senders to UPnP/Sonos and Chromecast devices.
//! This module resolves, spawns, and supervises AirConnect sidecars with
//! Windows Job Object assignment (so child processes never orphan).

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static AIRCONNECT_CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Look for bundled or installed AirConnect binary (`airupnp.exe` or `aircast.exe`).
pub fn find_airconnect_binary(app: &AppHandle, bin_name: &str) -> Option<PathBuf> {
    // 1. Check bundled engines directory
    if let Ok(res_dir) = app.path().resource_dir() {
        let p = res_dir.join("engines").join(bin_name);
        if p.is_file() {
            return Some(p);
        }
    }

    // 2. Check next to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let p = parent.join("engines").join(bin_name);
            if p.is_file() {
                return Some(p);
            }
            let p_direct = parent.join(bin_name);
            if p_direct.is_file() {
                return Some(p_direct);
            }
        }
    }

    // 3. Check current working directory
    let cwd_path = PathBuf::from("engines").join(bin_name);
    if cwd_path.is_file() {
        return Some(cwd_path);
    }

    None
}

/// Start an AirConnect sidecar bridge (e.g. `airupnp.exe` or `aircast.exe`).
pub fn start_airconnect(app: &AppHandle, bin_name: &str, extra_args: &[&str]) -> anyhow::Result<()> {
    stop_airconnect();

    let path = find_airconnect_binary(app, bin_name)
        .ok_or_else(|| anyhow::anyhow!("AirConnect binary '{bin_name}' not found"))?;

    let mut cmd = Command::new(path);
    cmd.args(extra_args);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn AirConnect sidecar: {e}"))?;

    #[cfg(windows)]
    crate::engines::job::assign(&child);

    let mut lock = AIRCONNECT_CHILD.lock().unwrap();
    *lock = Some(child);

    Ok(())
}

/// Stop running AirConnect sidecar.
pub fn stop_airconnect() {
    let mut lock = AIRCONNECT_CHILD.lock().unwrap();
    if let Some(mut child) = lock.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}
