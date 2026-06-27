use crate::format_handlers::ImageFormat;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResizeMode {
    Percent,
    Max,
}

impl Default for ResizeMode {
    fn default() -> Self {
        ResizeMode::Percent
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub save_location: PathBuf,
    pub threads_number: usize,
    pub resolution: u32,
    pub quality: u8,
    pub format: Option<ImageFormat>,
    pub resize_mode: ResizeMode,
    pub max_width: u32,
    pub max_height: u32,
    pub preserve_aspect: bool,
    pub resize_filter: ResizeFilter,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            save_location: PathBuf::from(r"C:\img"),
            threads_number: num_cpus::get(),
            resolution: 100,
            quality: 95,
            format: Some(ImageFormat::Jpg),
            resize_mode: ResizeMode::Percent,
            max_width: 3840,
            max_height: 2160,
            preserve_aspect: true,
            resize_filter: ResizeFilter::CatmullRom,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResizeFilter {
    Nearest,
    Triangle,
    CatmullRom,
    Lanczos3,
}

impl Default for ResizeFilter {
    fn default() -> Self {
        ResizeFilter::CatmullRom
    }
}

impl AppSettings {
    pub fn settings_path() -> Result<PathBuf> {
        let mut path = if cfg!(windows) {
            if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
                PathBuf::from(appdata)
            } else {
                dirs::data_local_dir().context("Failed to get local app data directory")?
            }
        } else {
            dirs::data_local_dir().context("Failed to get local app data directory")?
        };

        path.push("ResizerRust");
        fs::create_dir_all(&path).context("Failed to create settings directory")?;
        path.push("settings.json");
        Ok(path)
    }

    pub fn load() -> Result<Self> {
        let path = Self::settings_path()?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read settings from {:?}", path))?;

        let settings: AppSettings =
            serde_json::from_str(&content).with_context(|| "Failed to parse settings JSON")?;

        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::settings_path()?;

        let content = serde_json::to_string_pretty(self).context("Failed to serialize settings")?;

        fs::write(&path, content)
            .with_context(|| format!("Failed to write settings to {:?}", path))?;

        Ok(())
    }
}
