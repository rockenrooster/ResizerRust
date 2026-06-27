use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const OWNER: &str = "rockenrooster";
const REPO: &str = "ResizerRust";
const EXE_ASSET: &str = "resizerrust.exe";
const SHA_ASSET: &str = "resizerrust.exe.sha256";
const GITHUB_API_VERSION: &str = "2022-11-28";

static RESTART_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    url: String,
}

pub fn restart_started() -> bool {
    RESTART_STARTED.load(Ordering::Relaxed)
}

pub fn run_installer_if_requested() -> Result<bool> {
    let mut args = std::env::args_os();
    let _exe = args.next();
    if args.next().as_deref() != Some(OsStr::new("--resizerrust-install-update")) {
        return Ok(false);
    }

    let target_exe = args.next().context("missing update target path")?;
    if let Err(err) = install_from_current_exe(&PathBuf::from(target_exe)) {
        show_update_error(&format!("{err:#}"));
    }
    Ok(true)
}

pub async fn update_if_available() -> Result<()> {
    if cfg!(debug_assertions) {
        return Ok(());
    }

    let current = env!("CARGO_PKG_VERSION");
    let client = reqwest::Client::builder()
        .default_headers(default_headers()?)
        .timeout(Duration::from_secs(15))
        .build()?;

    let release: GitHubRelease = request(&client, latest_release_url())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if !version_is_newer(&release.tag_name, current) {
        return Ok(());
    }

    if !confirm_update(current, &release.tag_name) {
        return Ok(());
    }

    let exe = asset(&release, EXE_ASSET)?;
    let sha = asset(&release, SHA_ASSET)?;
    let exe_bytes = download_asset(&client, exe).await?;
    let expected_sha = expected_sha(&String::from_utf8(download_asset(&client, sha).await?)?)?;
    let actual_sha = sha256_hex(&exe_bytes);
    if actual_sha != expected_sha {
        bail!("downloaded update hash mismatch");
    }

    let target_exe = std::env::current_exe().context("current exe path")?;
    let update_exe = write_update_exe(&release.tag_name, &exe_bytes)?;
    if let Err(err) = start_installer(&update_exe, &target_exe) {
        show_update_error(&format!("{err:#}"));
        return Ok(());
    }
    RESTART_STARTED.store(true, Ordering::Relaxed);
    Ok(())
}

fn default_headers() -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("ResizerRust updater"));
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );
    Ok(headers)
}

fn request(client: &reqwest::Client, url: String) -> reqwest::RequestBuilder {
    let request = client
        .get(url)
        .header(ACCEPT, "application/vnd.github+json");
    match std::env::var("RESIZERRUST_GITHUB_TOKEN") {
        Ok(token) if !token.trim().is_empty() => request.bearer_auth(token),
        _ => request,
    }
}

fn latest_release_url() -> String {
    format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest")
}

fn asset<'a>(release: &'a GitHubRelease, name: &str) -> Result<&'a GitHubAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(name))
        .with_context(|| format!("release asset not found: {name}"))
}

async fn download_asset(client: &reqwest::Client, asset: &GitHubAsset) -> Result<Vec<u8>> {
    Ok(request(client, asset.url.clone())
        .header(ACCEPT, "application/octet-stream")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

fn write_update_exe(tag: &str, exe: &[u8]) -> Result<PathBuf> {
    let clean_tag: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = std::env::temp_dir().join(format!("resizerrust-update-{clean_tag}.exe"));
    std::fs::write(&path, exe)?;
    Ok(path)
}

fn expected_sha(text: &str) -> Result<String> {
    let Some(sha) = text.split_whitespace().next() else {
        bail!("empty sha256 file");
    };
    let sha = sha.to_ascii_lowercase();
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid sha256 file");
    }
    Ok(sha)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn confirm_update(current: &str, latest: &str) -> bool {
    MessageDialog::new()
        .set_level(MessageLevel::Info)
        .set_title("ResizerRust update available")
        .set_description(format!(
            "Version {latest} is available.\n\nCurrent version: {current}\n\nDownload and install it now? ResizerRust will close and relaunch."
        ))
        .set_buttons(MessageButtons::YesNo)
        .show()
        == MessageDialogResult::Yes
}

fn parse_version(value: &str) -> Option<[u64; 3]> {
    let value = value.trim().trim_start_matches('v');
    let mut parts = value.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([major, minor, patch])
}

#[cfg(windows)]
fn start_installer(update_exe: &Path, target_exe: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    Command::new(update_exe)
        .arg("--resizerrust-install-update")
        .arg(target_exe)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("starting updater")?;

    Ok(())
}

#[cfg(not(windows))]
fn start_installer(_update_exe: &Path, _target_exe: &Path) -> Result<()> {
    bail!("automatic replacement is only supported on Windows");
}

fn install_from_current_exe(target_exe: &Path) -> Result<()> {
    let source_exe = std::env::current_exe().context("current updater path")?;
    let source_hash = sha256_hex(&std::fs::read(&source_exe)?);
    let mut last_error = None;

    for _ in 0..60 {
        match std::fs::copy(&source_exe, target_exe) {
            Ok(_) => {
                let installed_hash = sha256_hex(&std::fs::read(target_exe)?);
                if installed_hash == source_hash {
                    launch_installed(target_exe)?;
                    return Ok(());
                }
                last_error = Some("installed file hash did not match downloaded update".to_string());
            }
            Err(err) => last_error = Some(err.to_string()),
        }
        thread::sleep(Duration::from_secs(1));
    }

    bail!(
        "could not replace {}\n{}",
        target_exe.display(),
        last_error.unwrap_or_else(|| "unknown error".to_string())
    )
}

fn launch_installed(target_exe: &Path) -> Result<()> {
    let mut command = Command::new(target_exe);
    if let Some(parent) = target_exe.parent() {
        command.current_dir(parent);
    }
    command.spawn().context("relaunching updated app")?;
    Ok(())
}

fn show_update_error(message: &str) {
    MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title("ResizerRust update failed")
        .set_description(format!(
            "ResizerRust could not install the update.\n\n{message}"
        ))
        .set_buttons(MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::{expected_sha, version_is_newer};

    #[test]
    fn compares_three_part_versions() {
        assert!(version_is_newer("v1.0.1", "1.0.0"));
        assert!(version_is_newer("2.0.0", "1.99.99"));
        assert!(!version_is_newer("v1.0.0", "1.0.0"));
        assert!(!version_is_newer("v1.0", "1.0.0"));
    }

    #[test]
    fn parses_sha256_file() {
        let sha = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            expected_sha(&format!("{sha}  resizerrust.exe")).unwrap(),
            sha
        );
        assert!(expected_sha("not-a-sha").is_err());
    }
}
