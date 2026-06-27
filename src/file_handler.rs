use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use filetime::FileTime;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct QueuedFile {
    pub input_path: PathBuf,
    pub relative_path: String,
}

pub fn get_file_size(path: &Path) -> Result<u64> {
    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to get metadata for {:?}", path))?;
    Ok(metadata.len())
}

pub fn extract_date_taken(path: &Path) -> Result<DateTime<Utc>> {
    // EXIF extraction will be implemented in Phase 5
    // For now, just use file modification time
    let metadata =
        fs::metadata(path).with_context(|| format!("Failed to get metadata for {:?}", path))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("Failed to get modified time for {:?}", path))?;

    Ok(DateTime::<Utc>::from(modified))
}

pub fn set_file_timestamp(path: &Path, dt: DateTime<Utc>) -> Result<()> {
    let file_time = FileTime::from_system_time(dt.into());
    filetime::set_file_mtime(path, file_time)
        .with_context(|| format!("Failed to set timestamp for {:?}", path))?;
    Ok(())
}

pub fn compute_relative_path(input_path: &Path, base_dir: &Path) -> Result<String> {
    let relative = input_path.strip_prefix(base_dir).with_context(|| {
        format!(
            "Failed to compute relative path from {:?} to {:?}",
            input_path, base_dir
        )
    })?;
    Ok(relative.to_string_lossy().to_string())
}

pub fn validate_file_format(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    matches!(
        ext.as_deref(),
        Some("jpg")
            | Some("jpeg")
            | Some("png")
            | Some("webp")
            | Some("bmp")
            | Some("gif")
            | Some("tiff")
            | Some("tif")
            | Some("ico")
            | Some("heif")
            | Some("heic")
            | Some("jxl")
            | Some("avif")
    )
}

pub fn scan_directory(dir: &Path) -> Result<Vec<QueuedFile>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read directory {:?}", dir))? {
        let entry = entry.with_context(|| format!("Failed to read entry in {:?}", dir))?;
        let path = entry.path();

        if path.is_dir() {
            let sub_files = scan_directory(&path)?;
            files.extend(sub_files);
        } else if path.is_file() && validate_file_format(&path) {
            let relative = compute_relative_path(&path, dir)?;
            files.push(QueuedFile {
                input_path: path,
                relative_path: relative,
            });
        }
    }

    Ok(files)
}

pub fn ensure_output_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {:?}", parent))?;
        }
    }
    Ok(())
}

pub fn generate_output_path(
    input_path: &Path,
    relative_path: &str,
    output_root: &Path,
    format: &str,
    preserve_structure: bool,
) -> Result<PathBuf> {
    let filename_stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    if preserve_structure {
        let root = Path::new(input_path.components().next().unwrap().as_os_str());
        let safe_path = input_path.strip_prefix(root).unwrap_or(input_path);

        let new_filename = format!("{}.{}", filename_stem, format);
        Ok(output_root.join(safe_path).with_file_name(new_filename))
    } else {
        if let Some(parent) = Path::new(relative_path).parent() {
            let new_filename = format!("{}.{}", filename_stem, format);
            Ok(output_root.join(parent).join(new_filename))
        } else {
            Ok(output_root.join(format!("{}.{}", filename_stem, format)))
        }
    }
}
