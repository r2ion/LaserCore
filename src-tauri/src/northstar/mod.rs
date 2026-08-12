//! This module deals with handling things around Northstar such as
//! - getting version number
pub mod install;
pub mod profile;

use crate::constants::{CORE_MODS, ION_REPO_NAME, ION_REPO_OWNER};
use crate::util::check_ea_app_or_origin_running;
use crate::{platform_specific::get_host_os, GameInstall, InstallType};
use crate::{IonRelease, IonReleaseWrapper};
use anyhow::anyhow;
use octocrab::models::repos::{Asset, Release};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct IonVersion {
    version: semver::Version,
    patch: Option<u64>,
}

impl std::fmt::Display for IonVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.patch {
            Some(patch) => write!(formatter, "{}-{}", self.version, patch),
            None => write!(formatter, "{}", self.version),
        }
    }
}

pub(crate) fn ion_version_from_release_tag(tag: &str) -> Result<IonVersion, String> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let (version, patch) = match tag.rsplit_once('_') {
        Some((version, patch)) => {
            let patch = patch
                .parse::<u64>()
                .map_err(|_| format!("Invalid Ion release patch in tag \"{tag}\""))?;
            (version, Some(patch))
        }
        None => (tag, None),
    };
    let version = semver::Version::parse(version)
        .map_err(|err| format!("Invalid Ion release tag \"{tag}\": {err}"))?;

    Ok(IonVersion { version, patch })
}

pub(crate) fn ion_release_asset(release: &Release) -> Result<&Asset, String> {
    let mut matching_assets = release
        .assets
        .iter()
        .filter(|asset| asset.name.starts_with("Ion.release.") && asset.name.ends_with(".zip"));
    let asset = matching_assets.next().ok_or_else(|| {
        format!(
            "Ion GitHub release {} does not contain an Ion release archive",
            release.tag_name
        )
    })?;

    if matching_assets.next().is_some() {
        return Err(format!(
            "Ion GitHub release {} contains multiple Ion release archives",
            release.tag_name
        ));
    }

    Ok(asset)
}

pub(crate) async fn get_ion_release(tag: Option<&str>) -> Result<Release, String> {
    let github = octocrab::instance();
    let repository = github.repos(ION_REPO_OWNER, ION_REPO_NAME);
    let releases = repository.releases();
    let release = match tag {
        Some(tag) => releases.get_by_tag(tag).await,
        None => releases.get_latest().await,
    };

    release.map_err(|err| format!("Failed to fetch Ion release from GitHub: {err}"))
}

#[derive(Serialize, Deserialize, Debug, Clone, TS)]
#[ts(export)]
pub struct NorthstarLaunchOptions {
    launch_via_steam: bool,
    bypass_checks: bool,
}

/// Gets installable Ion releases from GitHub.
#[tauri::command]
pub async fn get_available_northstar_versions() -> Result<Vec<IonReleaseWrapper>, String> {
    let github = octocrab::instance();
    let repository = github.repos(ION_REPO_OWNER, ION_REPO_NAME);
    let page = repository
        .releases()
        .list()
        .per_page(100)
        .page(1u32)
        .send()
        .await
        .map_err(|err| format!("Failed to fetch Ion releases from GitHub: {err}"))?;

    let mut releases = Vec::new();
    for release in page.items {
        if release.draft || release.prerelease {
            continue;
        }

        let version = match ion_version_from_release_tag(&release.tag_name) {
            Ok(version) => version,
            Err(err) => {
                log::warn!("Skipping Ion release: {err}");
                continue;
            }
        };
        if let Err(err) = ion_release_asset(&release) {
            log::warn!("Skipping Ion release: {err}");
            continue;
        }

        let label = release.name.unwrap_or_else(|| format!("Ion v{version}"));
        let version_string = version.to_string();
        releases.push((
            version,
            IonReleaseWrapper {
                label,
                value: IonRelease {
                    tag: release.tag_name,
                    version: version_string,
                },
            },
        ));
    }

    releases.sort_by(|(left, _), (right, _)| right.cmp(left));
    Ok(releases.into_iter().map(|(_, release)| release).collect())
}

/// Checks whether the installed Ion version differs from the latest GitHub release.
#[tauri::command]
pub async fn check_is_northstar_outdated(game_install: GameInstall) -> Result<bool, String> {
    let latest_release = get_ion_release(None).await?;
    ion_release_asset(&latest_release)?;
    let latest_version = ion_version_from_release_tag(&latest_release.tag_name)?.to_string();
    log::info!("Latest Ion version on GitHub: {latest_version}");

    let installed_version = get_northstar_version_number(game_install)?;
    log::info!("Installed Ion version: {installed_version}");

    if installed_version != latest_version {
        log::info!("Installed Ion version outdated");
        Ok(true)
    } else {
        log::info!("Installed Ion version up-to-date");
        Ok(false)
    }
}

/// Check version number of a mod
pub fn check_mod_version_number(path_to_mod_folder: &str) -> Result<String, anyhow::Error> {
    let data = std::fs::read_to_string(format!("{path_to_mod_folder}/mod.json"))?;
    let parsed_json: serde_json::Value = serde_json::from_str(&data)?;

    let mod_version_number = match parsed_json.get("Version").and_then(|value| value.as_str()) {
        Some(version_number) => version_number,
        None => {
            return Err(anyhow!("No version number found"));
        }
    };

    log::info!("{}", mod_version_number);

    Ok(mod_version_number.to_string())
}

pub fn check_mod_patch_number(path_to_mod_folder: &str) -> Result<String, anyhow::Error> {
    let data = std::fs::read_to_string(format!("{path_to_mod_folder}/mod.json"))?;
    let parsed_json: serde_json::Value = serde_json::from_str(&data)?;

    match parsed_json.get("Patch") {
        Some(value) => {
            // Handle both string and number types
            let patch_str = if let Some(s) = value.as_str() {
                s.to_string()
            } else if let Some(n) = value.as_i64() {
                n.to_string()
            } else if let Some(n) = value.as_u64() {
                n.to_string()
            } else {
                return Err(anyhow!("Patch field has unexpected type"));
            };
            Ok(patch_str)
        }
        None => Err(anyhow!("No patch number found")),
    }
}

/// Returns the current Northstar version number as a string
/// The version includes the patch number if available (format: "version-patch" or just "version")
#[tauri::command]
pub fn get_northstar_version_number(game_install: GameInstall) -> Result<String, String> {
    log::info!("{}", game_install.game_path);

    // TODO:
    // Check if NorthstarLauncher.exe exists and check its version number
    let first_mod_path = format!(
        "{}/{}/mods/{}",
        game_install.game_path, game_install.profile, CORE_MODS[0]
    );

    let initial_version_number = match check_mod_version_number(&first_mod_path) {
        Ok(version_number) => version_number,
        Err(err) => {
            return Err(err.to_string());
        }
    };

    log::info!("Initial mod version: {}", initial_version_number);

    let initial_patch_number = check_mod_patch_number(&first_mod_path).ok();
    log::info!("Initial patch: {:?}", initial_patch_number);
    for core_mod in CORE_MODS {
        let mod_path = format!(
            "{}/{}/mods/{}",
            game_install.game_path, game_install.profile, core_mod
        );

        let current_version_number = match check_mod_version_number(&mod_path) {
            Ok(version_number) => version_number,
            Err(err) => {
                return Err(err.to_string());
            }
        };
        if current_version_number != initial_version_number {
            // We have a version number mismatch
            return Err("Found version number mismatch".to_string());
        }

        let current_patch_number = check_mod_patch_number(&mod_path).ok();
        if current_patch_number != initial_patch_number {
            // We have a patch number mismatch
            return Err("Found patch number mismatch".to_string());
        }
    }
    log::info!("All mods same version and patch");

    // Combine version and patch if patch is available
    let full_version = match initial_patch_number {
        Some(patch) => format!("{}-{}", initial_version_number, patch),
        None => initial_version_number,
    };

    log::info!("Full Northstar version: {}", full_version);

    Ok(full_version)
}

/// Launches Northstar
#[tauri::command]
pub fn launch_northstar(
    game_install: GameInstall,
    launch_options: NorthstarLaunchOptions,
) -> Result<String, String> {
    dbg!(game_install.clone());

    if launch_options.launch_via_steam {
        return launch_northstar_steam(game_install);
    }

    let host_os = get_host_os();

    // Explicitly fail early certain (currently) unsupported install setups
    if host_os != "windows" {
        if !matches!(game_install.install_type, InstallType::STEAM) {
            return Err(format!(
                "Not yet implemented for \"{}\" with Titanfall2 installed via \"{:?}\"",
                get_host_os(),
                game_install.install_type
            ));
        }

        return launch_northstar_steam(game_install);
    }

    // Only check guards if bypassing checks is not enabled
    if !launch_options.bypass_checks {
        // Some safety checks before, should have more in the future
        if get_northstar_version_number(game_install.clone()).is_err() {
            return Err(anyhow!("Not all checks were met").to_string());
        }

        // Require EA App or Origin to be running to launch Northstar
        let ea_app_is_running = check_ea_app_or_origin_running();
        if !ea_app_is_running {
            return Err(
                anyhow!("EA App not running, start EA App before launching Northstar").to_string(),
            );
        }
    }

    // Switch to Titanfall2 directory for launching
    // NorthstarLauncher.exe expects to be run from that folder
    if std::env::set_current_dir(game_install.game_path.clone()).is_err() {
        // We failed to get to Titanfall2 directory
        return Err(anyhow!("Couldn't access Titanfall2 directory").to_string());
    }

    // Only Windows with Steam or Origin are supported at the moment
    if host_os == "windows"
        && (matches!(game_install.install_type, InstallType::STEAM)
            || matches!(game_install.install_type, InstallType::ORIGIN)
            || matches!(game_install.install_type, InstallType::UNKNOWN))
    {
        let ns_exe_path = format!("{}/NorthstarLauncher.exe", game_install.game_path);
        let ns_profile_arg = format!("-profile={}", game_install.profile);

        let mut output = std::process::Command::new("C:\\Windows\\System32\\cmd.exe")
            .args(["/C", "start", "", &ns_exe_path, &ns_profile_arg])
            .spawn()
            .expect("failed to execute process");
        output.wait().expect("failed waiting on child process");
        return Ok("Launched game".to_string());
    }

    Err(format!(
        "Not yet implemented for {:?} on {}",
        game_install.install_type,
        get_host_os()
    ))
}

/// Prepare Northstar and Launch through Steam using the Browser Protocol
pub fn launch_northstar_steam(game_install: GameInstall) -> Result<String, String> {
    if !matches!(game_install.install_type, InstallType::STEAM) {
        return Err("Titanfall2 was not installed via Steam".to_string());
    }

    match steamlocate::SteamDir::locate() {
        Ok(steamdir) => {
            if get_host_os() != "windows" {
                match steamdir.compat_tool_mapping() {
                    Ok(map) => match map.get(&thermite::TITANFALL2_STEAM_ID) {
                        Some(_) => {}
                        None => {
                            return Err(
                                "Titanfall2 was not configured to use a compatibility tool"
                                    .to_string(),
                            );
                        }
                    },
                    Err(_) => {
                        return Err("Could not get compatibility tool mapping".to_string());
                    }
                }
            }
        }
        Err(_) => {
            return Err("Couldn't access Titanfall2 directory".to_string());
        }
    }

    // Switch to Titanfall2 directory to set everything up
    if std::env::set_current_dir(game_install.game_path).is_err() {
        // We failed to get to Titanfall2 directory
        return Err("Couldn't access Titanfall2 directory".to_string());
    }

    match open::that(format!(
        "steam://run/{}//-profile={} --northstar/",
        thermite::TITANFALL2_STEAM_ID,
        game_install.profile
    )) {
        Ok(()) => Ok("Started game".to_string()),
        Err(_err) => Err("Failed to launch Titanfall 2 via Steam".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::ion_version_from_release_tag;

    #[test]
    fn parses_patched_ion_release_tag() {
        let version = ion_version_from_release_tag("v1.31.11_2").unwrap();
        assert_eq!(version.to_string(), "1.31.11-2");
    }

    #[test]
    fn parses_legacy_ion_release_tag() {
        let version = ion_version_from_release_tag("v1.6.3").unwrap();
        assert_eq!(version.to_string(), "1.6.3");
    }

    #[test]
    fn rejects_non_numeric_ion_patch() {
        let error = ion_version_from_release_tag("v1.31.11_patch").unwrap_err();
        assert!(error.contains("Invalid Ion release patch"));
    }
}
