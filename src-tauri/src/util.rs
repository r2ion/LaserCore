//! This module contains various utility/helper functions that do not fit into any other module

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::constants::{APP_USER_AGENT, MASTER_SERVER_URL, SERVER_BROWSER_ENDPOINT};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NorthstarServer {
    #[serde(rename = "playerCount")]
    pub player_count: i32,
}

/// This function's only use is to force a `panic!()`
// This must NOT be async to ensure crashing whole application.
#[tauri::command]
pub fn force_panic() {
    panic!("Force panicked!");
}

/// Returns true if built in debug mode
#[tauri::command]
pub async fn is_debug_mode() -> bool {
    cfg!(debug_assertions)
}

/// Returns the current version number as a string
#[tauri::command]
pub async fn get_flightcore_version_number() -> String {
    let version = env!("CARGO_PKG_VERSION");
    if cfg!(debug_assertions) {
        // Debugging enabled
        format!("v{version} (debug mode)")
    } else {
        // Debugging disabled
        format!("v{version}")
    }
}

/// Spawns repair window
#[tauri::command]
pub async fn open_repair_window(handle: tauri::AppHandle) -> Result<(), String> {
    // Spawn new window
    let repair_window = match tauri::WebviewWindowBuilder::new(
        &handle,
        "RepairWindow",
        tauri::WebviewUrl::App("/#/repair".into()),
    )
    .build()
    {
        Ok(res) => res,
        Err(err) => {
            return Err(err.to_string());
        }
    };

    // Set window title
    match repair_window.set_title("FlightCore Repair Window") {
        Ok(()) => (),
        Err(err) => {
            return Err(err.to_string());
        }
    }
    Ok(())
}

/// Closes all windows and exits application
#[tauri::command]
pub async fn close_application<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    app.exit(0); // Close application
    Ok(())
}

/// Fetches `/client/servers` endpoint from master server
async fn fetch_server_list() -> Result<String, anyhow::Error> {
    let url = format!("{MASTER_SERVER_URL}{SERVER_BROWSER_ENDPOINT}");
    let client = reqwest::Client::new();
    let res = client
        .get(url)
        .header(reqwest::header::USER_AGENT, APP_USER_AGENT)
        .send()
        .await?
        .text()
        .await?;

    Ok(res)
}

/// Gets server and playercount from master server API
#[tauri::command]
pub async fn get_server_player_count() -> Result<(i32, usize), String> {
    let res = match fetch_server_list().await {
        Ok(res) => res,
        Err(err) => {
            return Err(err.to_string());
        }
    };

    let ns_servers: Vec<NorthstarServer> =
        serde_json::from_str(&res).expect("JSON was not well-formatted");

    // Get server count
    let server_count = ns_servers.len();

    // Sum up player count
    let total_player_count: i32 = ns_servers.iter().map(|server| server.player_count).sum();

    log::info!("total_player_count: {}", total_player_count);
    log::info!("server_count:       {}", server_count);

    Ok((total_player_count, server_count))
}

#[tauri::command]
pub async fn kill_northstar() -> Result<(), String> {
    if !check_northstar_running() {
        return Err("Northstar is not running".to_string());
    }

    let s = sysinfo::System::new_all();

    for process in s.processes_by_exact_name("Titanfall2.exe") {
        log::info!("Killing Process {}", process.pid());
        process.kill();
    }

    for process in s.processes_by_exact_name("NorthstarLauncher.exe") {
        log::info!("Killing Process {}", process.pid());
        process.kill();
    }

    Ok(())
}

/// Extracts a GitHub Ion release archive into a staging directory.
pub fn extract_ion_release<R>(zip_file: R, target: &std::path::Path) -> Result<()>
where
    R: std::io::Read + std::io::Seek,
{
    let mut archive = ZipArchive::new(zip_file).context("Unable to open Ion release archive")?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("Unable to read Ion release archive entry")?;
        let relative_path = entry
            .enclosed_name()
            .ok_or_else(|| {
                anyhow::anyhow!("Ion release contains an unsafe path: {}", entry.name())
            })?
            .to_path_buf();
        let output_path = target.join(relative_path);

        if entry.is_dir() {
            log::info!("Create directory {}", output_path.display());
            std::fs::create_dir_all(&output_path).context("Unable to create directory")?;
            continue;
        }

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).context("Unable to create directory")?;
        }

        let mut output_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_path)?;

        log::info!("Write file {}", output_path.display());
        std::io::copy(&mut entry, &mut output_file).context("Unable to write file")?;
    }

    Ok(())
}

pub fn check_ea_app_or_origin_running() -> bool {
    let s = sysinfo::System::new_all();
    let x = s.processes_by_name("Origin.exe").next().is_some()
        || s.processes_by_name("EADesktop.exe").next().is_some();
    x
}

/// Checks if Northstar process is running
pub fn check_northstar_running() -> bool {
    let s = sysinfo::System::new_all();
    let x = s
        .processes_by_name("NorthstarLauncher.exe")
        .next()
        .is_some()
        || s.processes_by_name("Titanfall2.exe").next().is_some();
    x
}

/// Copies a folder and all its contents to a new location
pub fn copy_dir_all(
    src: impl AsRef<std::path::Path>,
    dst: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Moves a folders file structure to a new location
/// Old folders are not removed
pub fn move_dir_all(
    src: impl AsRef<std::path::Path>,
    dst: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            move_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
            std::fs::remove_dir(entry.path())?;
        } else {
            std::fs::rename(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::extract_ion_release;
    use std::io::{Cursor, Write};

    #[test]
    fn extracts_direct_github_release_layout() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file(
                "R2Northstar/mods/Northstar.Client/mod.json",
                zip::write::FileOptions::default(),
            )
            .unwrap();
        archive
            .write_all(br#"{"Version":"1.31.11","Patch":"2"}"#)
            .unwrap();
        let archive = archive.finish().unwrap();

        let target = std::env::temp_dir().join(format!(
            "lasercore-ion-extract-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        extract_ion_release(archive, &target).unwrap();

        let extracted =
            std::fs::read_to_string(target.join("R2Northstar/mods/Northstar.Client/mod.json"))
                .unwrap();
        assert_eq!(extracted, r#"{"Version":"1.31.11","Patch":"2"}"#);

        std::fs::remove_dir_all(target).unwrap();
    }
}
