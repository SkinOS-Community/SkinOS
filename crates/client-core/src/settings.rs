use crate::session;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use skinos_compat::GameSettings;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub steamapps_dir: PathBuf,
    pub crossover_path: Option<PathBuf>,
    #[serde(default)]
    pub download_region_id: Option<u32>,
}

pub fn load() -> Result<Settings> {
    let path = session::data_dir()?.join("settings.json");
    if path.exists() {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    } else {
        Ok(Settings {
            steamapps_dir: PathBuf::from(
                std::env::var_os("HOME").context("User home unavailable")?,
            )
            .join("Library/Application Support/Steam/steamapps"),
            crossover_path: None,
            download_region_id: None,
        })
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        skinos_downloader::validate_download_region(self.download_region_id)?;
        ensure!(
            self.steamapps_dir.is_absolute(),
            "Выберите абсолютный путь к steamapps"
        );
        ensure!(
            self.steamapps_dir
                .file_name()
                .is_some_and(|n| n == "steamapps"),
            "Каталог библиотеки должен называться steamapps"
        );
        if let Some(path) = &self.crossover_path {
            validate_crossover(path)?;
        }
        Ok(())
    }
}
fn validate_crossover(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.extension().is_some_and(|e| e == "app")
            && path
                .join("Contents/SharedSupport/CrossOver/lib/wine")
                .is_dir(),
        "Выберите установленное приложение CrossOver или CrossOver Preview"
    );
    Ok(())
}

pub fn save(value: &Settings) -> Result<()> {
    value.validate()?;
    fs::create_dir_all(value.steamapps_dir.join("common"))?;
    write_json(&session::data_dir()?.join("settings.json"), value)
}

pub fn game(appid: u32) -> Result<GameSettings> {
    let path = game_path(appid)?;
    if path.exists() {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    } else {
        Ok(GameSettings::default())
    }
}

pub fn save_game(appid: u32, value: &GameSettings) -> Result<()> {
    ensure!(appid > 0, "Invalid AppID");
    value.validate().map_err(anyhow::Error::msg)?;
    if let Some(path) = &value.crossover_path {
        validate_crossover(path)?;
    }
    write_json(&game_path(appid)?, value)
}

fn game_path(appid: u32) -> Result<PathBuf> {
    Ok(session::data_dir()?
        .join("game-settings")
        .join(format!("{appid}.json")))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    fs::create_dir_all(path.parent().context("Settings parent missing")?)?;
    ensure!(!path.is_symlink(), "Settings file cannot be a symlink");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = path.with_extension(format!("{}-{nonce}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_settings_reject_wrong_roots_and_fake_crossover() {
        for root in ["relative/steamapps", "/tmp/common", "/"] {
            assert!(
                Settings {
                    steamapps_dir: PathBuf::from(root),
                    crossover_path: None,
                    download_region_id: None,
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            Settings {
                steamapps_dir: PathBuf::from("/tmp/library/steamapps"),
                crossover_path: None,
                download_region_id: None,
            }
            .validate()
            .is_ok()
        );
        assert!(
            Settings {
                steamapps_dir: PathBuf::from("/tmp/steamapps"),
                crossover_path: Some(PathBuf::from("/Applications")),
                download_region_id: None,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn legacy_settings_default_to_automatic_download_region() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "steamapps_dir": "/tmp/library/steamapps",
            "crossover_path": null
        }))
        .unwrap();
        assert_eq!(settings.download_region_id, None);
        settings.validate().unwrap();
    }

    #[test]
    fn selected_and_automatic_download_regions_roundtrip_to_disk() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        for download_region_id in [Some(165), None] {
            let settings = Settings {
                steamapps_dir: temp.path().join("steamapps"),
                crossover_path: None,
                download_region_id,
            };
            settings.validate().unwrap();
            write_json(&path, &settings).unwrap();
            let loaded: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(loaded.download_region_id, download_region_id);
            assert_eq!(loaded.steamapps_dir, settings.steamapps_dir);
            assert_eq!(loaded.crossover_path, settings.crossover_path);
        }
    }

    #[test]
    fn invalid_download_region_is_rejected_before_creating_library_directory() {
        let temp = tempfile::tempdir().unwrap();
        let steamapps_dir = temp.path().join("steamapps");
        for download_region_id in [Some(0), Some(u32::MAX)] {
            let settings = Settings {
                steamapps_dir: steamapps_dir.clone(),
                crossover_path: None,
                download_region_id,
            };
            // Invalid settings fail before save resolves the user's data dir.
            assert!(save(&settings).is_err());
            assert!(!steamapps_dir.exists());
        }
    }
}
