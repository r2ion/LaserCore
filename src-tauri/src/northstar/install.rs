use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use ts_rs::TS;

use crate::constants::{APP_USER_AGENT, CORE_MODS, NORTHSTAR_DEFAULT_PROFILE, NORTHSTAR_DLL};
use crate::northstar::{get_ion_release, ion_release_asset, ion_version_from_release_tag};
use crate::util::{extract_ion_release, move_dir_all};
use crate::{GameInstall, InstallType};

#[cfg(target_os = "windows")]
use crate::platform_specific::windows;

#[derive(Serialize, Deserialize, Debug, Clone, TS)]
#[ts(export)]
enum InstallState {
    Downloading,
    Extracting,
    Done,
}

#[derive(Serialize, Deserialize, Debug, Clone, TS)]
#[ts(export)]
struct InstallProgress {
    current_downloaded: u64,
    total_size: u64,
    state: InstallState,
}

fn validate_staged_ion_release(extract_directory: &str) -> Result<()> {
    let release_root = Path::new(extract_directory);

    for required_file in [NORTHSTAR_DLL, "NorthstarLauncher.exe"] {
        let path = release_root.join(required_file);
        if !path.is_file() {
            return Err(anyhow!(
                "Ion release archive is missing required file {}",
                path.display()
            ));
        }
    }

    for core_mod in CORE_MODS {
        let mod_json = release_root
            .join(NORTHSTAR_DEFAULT_PROFILE)
            .join("mods")
            .join(core_mod)
            .join("mod.json");
        if !mod_json.is_file() {
            return Err(anyhow!(
                "Ion release archive is missing required core mod {}",
                mod_json.display()
            ));
        }
    }

    Ok(())
}

/// Installs Ion to the given path from a GitHub release.
#[tauri::command]
pub async fn install_northstar_wrapper(
    app: AppHandle,
    game_install: GameInstall,
    release_tag: Option<String>,
) -> Result<bool, String> {
    log::info!("Running Ion install");
    let release_tag = release_tag.filter(|tag| !tag.is_empty());

    match install_northstar(app, game_install, release_tag).await {
        Ok(_) => Ok(true),
        Err(err) => {
            log::error!("{}", err);
            Err(err)
        }
    }
}

/// Updates Ion to the latest GitHub release.
#[tauri::command]
pub async fn update_northstar(app: AppHandle, game_install: GameInstall) -> Result<bool, String> {
    log::info!("Updating Ion");
    install_northstar_wrapper(app, game_install, None).await
}

/// Downloads and installs an Ion GitHub release archive.
async fn do_install(
    app: AppHandle,
    download_url: &str,
    version: &str,
    game_install: GameInstall,
) -> Result<()> {
    let filename = format!("ion-{version}.zip");
    let temp_dir = format!("{}/___flightcore-temp", game_install.game_path);
    let download_directory = format!("{temp_dir}/download-dir");
    let extract_directory = format!("{temp_dir}/extract-dir");

    if Path::new(&temp_dir).exists() {
        log::info!("Removing stale temporary directory {temp_dir}");
        std::fs::remove_dir_all(&temp_dir)?;
    }

    log::info!("Attempting to create temporary directory {temp_dir}");
    std::fs::create_dir_all(&download_directory)?;
    std::fs::create_dir_all(&extract_directory)?;

    let download_path = format!("{download_directory}/{filename}");
    log::info!("Downloading Ion {version} from GitHub to {download_path}");

    let client = reqwest::Client::builder()
        .user_agent(APP_USER_AGENT)
        .build()?;
    let mut response = client.get(download_url).send().await?.error_for_status()?;
    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded = 0u64;
    let mut last_emit = Instant::now();
    let mut release_file = tokio::fs::File::create(&download_path).await?;

    while let Some(chunk) = response.chunk().await? {
        release_file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed() >= Duration::from_millis(100) {
            app.emit(
                "northstar-install-download-progress",
                InstallProgress {
                    current_downloaded: downloaded,
                    total_size,
                    state: InstallState::Downloading,
                },
            )?;
            last_emit = Instant::now();
        }
    }
    release_file.flush().await?;
    drop(release_file);

    if total_size != 0 && downloaded != total_size {
        return Err(anyhow!(
            "Incomplete Ion download: received {downloaded} of {total_size} bytes"
        ));
    }

    app.emit(
        "northstar-install-download-progress",
        InstallProgress {
            current_downloaded: downloaded,
            total_size: total_size.max(downloaded),
            state: InstallState::Downloading,
        },
    )?;
    app.emit(
        "northstar-install-download-progress",
        InstallProgress {
            current_downloaded: 0,
            total_size: 0,
            state: InstallState::Extracting,
        },
    )?;

    log::info!("Extracting Ion...");
    let release_file = std::fs::File::open(&download_path)?;
    extract_ion_release(release_file, Path::new(&extract_directory))?;
    validate_staged_ion_release(&extract_directory)?;

    log::info!("Preparing Ion...");
    if game_install.profile != NORTHSTAR_DEFAULT_PROFILE {
        let old_dll_path = format!("{extract_directory}/{NORTHSTAR_DLL}");
        let new_dll_path =
            format!("{extract_directory}/{NORTHSTAR_DEFAULT_PROFILE}/{NORTHSTAR_DLL}");
        std::fs::rename(old_dll_path, new_dll_path)?;

        let old_profile_path = format!("{extract_directory}/{NORTHSTAR_DEFAULT_PROFILE}/");
        let new_profile_path = format!("{}/{}/", extract_directory, game_install.profile);
        std::fs::rename(old_profile_path, new_profile_path)?;
    }

    log::info!("Installing Ion...");
    for core_mod in CORE_MODS {
        let path_to_delete_string = format!(
            "{}/{}/mods/{}/",
            game_install.game_path, game_install.profile, core_mod
        );
        log::info!("Preparing to remove {path_to_delete_string}");

        let path_to_delete = Path::new(&path_to_delete_string);
        if !path_to_delete.exists() {
            log::info!("{path_to_delete_string} does not exist. Skipping");
            continue;
        }
        if !path_to_delete.is_dir() {
            log::error!("{path_to_delete_string} exists but is a file. Skipping");
            continue;
        }

        let mod_json_path = path_to_delete.join("mod.json");
        if !mod_json_path.exists() {
            log::error!("Missing mod.json for {path_to_delete_string}. Skipping");
            continue;
        }

        match std::fs::remove_dir_all(path_to_delete) {
            Ok(()) => log::info!("Successfully removed {path_to_delete_string}"),
            Err(err) => {
                log::error!("Failed removing {path_to_delete_string} due to {err}");
            }
        }
    }

    for entry in std::fs::read_dir(&extract_directory)? {
        let entry = entry?;
        let destination = Path::new(&game_install.game_path).join(entry.file_name());

        log::info!("Installing {}", entry.path().display());
        if entry.file_type()?.is_dir() {
            move_dir_all(entry.path(), destination)?;
        } else {
            std::fs::rename(entry.path(), destination)?;
        }
    }

    log::info!("Deleting temporary directory");
    std::fs::remove_dir_all(temp_dir)?;

    log::info!("Done installing Ion!");
    app.emit(
        "northstar-install-download-progress",
        InstallProgress {
            current_downloaded: 0,
            total_size: 0,
            state: InstallState::Done,
        },
    )?;

    Ok(())
}

pub async fn install_northstar(
    app: AppHandle,
    game_install: GameInstall,
    release_tag: Option<String>,
) -> Result<String, String> {
    let release = get_ion_release(release_tag.as_deref()).await?;
    let version = ion_version_from_release_tag(&release.tag_name)?.to_string();
    let download_url = ion_release_asset(&release)?
        .browser_download_url
        .to_string();
    log::info!(
        "Selected Ion GitHub release {} ({version})",
        release.tag_name
    );

    let game_path = game_install.game_path.clone();
    log::info!("Install path \"{game_path}\"");

    if let Err(err) = do_install(app, &download_url, &version, game_install).await {
        if game_path
            .to_lowercase()
            .contains(&r"C:\Program Files\".to_lowercase())
        {
            return Err(
                "Cannot install to default EA App install path, please move Titanfall2 to a different install location.".to_string(),
            );
        }
        return Err(err.to_string());
    }

    Ok(version)
}

/// Attempts to find the game install location
#[tauri::command]
pub fn find_game_install_location() -> Result<GameInstall, String> {
    // Attempt parsing Steam library directly
    match steamlocate::SteamDir::locate() {
        Ok(steamdir) => {
            #[cfg(target_os = "linux")]
            {
                let snap_dir = match std::env::var("SNAP_USER_DATA") {
                    Ok(snap_dir) => std::path::PathBuf::from(snap_dir),
                    Err(_) => (match dirs::home_dir() {
                        Some(path) => path,
                        None => std::path::PathBuf::new(),
                    })
                    .join("snap"),
                };

                if steamdir.path().starts_with(snap_dir) {
                    log::warn!("Found Steam installed via Snap, you may encounter issues");
                }
            }

            match steamdir.find_app(thermite::TITANFALL2_STEAM_ID) {
                Ok(Some((app, library))) => {
                    let app_path = library
                        .path()
                        .join("steamapps")
                        .join("common")
                        .join(app.install_dir)
                        .into_os_string()
                        .into_string()
                        .unwrap();

                    let game_install = GameInstall {
                        game_path: app_path,
                        profile: "R2Northstar".to_string(),
                        install_type: InstallType::STEAM,
                    };
                    return Ok(game_install);
                }
                Ok(None) => log::info!("Couldn't locate your Titanfall 2 Steam install."),
                Err(err) => log::info!(
                    "Something went wrong while trying to find Titanfall 2 {}",
                    err
                ),
            }
        }
        Err(err) => log::info!("Couldn't locate Steam on this computer! {}", err),
    }

    // (On Windows only) try parsing Windows registry for Origin install path
    #[cfg(target_os = "windows")]
    match windows::origin_install_location_detection() {
        Ok(game_path) => {
            let game_install = GameInstall {
                game_path,
                profile: "R2Northstar".to_string(),
                install_type: InstallType::ORIGIN,
            };
            return Ok(game_install);
        }
        Err(err) => {
            log::info!("{}", err);
        }
    }

    Err("Could not auto-detect game install location! Please enter it manually.".to_string())
}
