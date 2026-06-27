use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
    start_replacer(&update_exe, &target_exe, release.tag_name.trim_start_matches('v'))?;
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
fn start_replacer(update_exe: &Path, target_exe: &Path, expected_version: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let work_dir = target_exe
        .parent()
        .context("target exe has no parent directory")?;
    let script_path =
        std::env::temp_dir().join(format!("resizerrust-updater-{}.ps1", std::process::id()));
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         $pidToWait = {}\n\
         $newExe = {}\n\
         $targetExe = {}\n\
         $workDir = {}\n\
         $expectedVersion = {}\n\
         try {{ Wait-Process -Id $pidToWait -Timeout 30 -ErrorAction SilentlyContinue }} catch {{}}\n\
         try {{\n\
             for ($i = 0; $i -lt 30; $i++) {{\n\
                 try {{\n\
                     Copy-Item -LiteralPath $newExe -Destination $targetExe -Force\n\
                     break\n\
                 }} catch {{\n\
                     Start-Sleep -Seconds 1\n\
                     if ($i -eq 29) {{ throw }}\n\
                 }}\n\
             }}\n\
             $installedVersion = (Get-Item -LiteralPath $targetExe).VersionInfo.ProductVersion\n\
             if ($installedVersion -notlike \"$expectedVersion*\") {{\n\
                 throw \"Installed version $installedVersion does not match $expectedVersion.\"\n\
             }}\n\
         }} catch {{\n\
             Add-Type -AssemblyName PresentationFramework\n\
             [System.Windows.MessageBox]::Show(\"ResizerRust could not install the update.`n`n$($_.Exception.Message)\", \"Update failed\", \"OK\", \"Error\") | Out-Null\n\
             exit 1\n\
         }}\n\
         Start-Process -FilePath $targetExe -WorkingDirectory $workDir\n\
         Start-Sleep -Seconds 2\n\
         Remove-Item -LiteralPath $newExe -Force -ErrorAction SilentlyContinue\n\
         Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue\n",
        std::process::id(),
        ps_quote(update_exe),
        ps_quote(target_exe),
        ps_quote(work_dir),
        ps_quote_text(expected_version),
    );
    std::fs::write(&script_path, script)?;

    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(script_path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("starting updater")?;

    Ok(())
}

#[cfg(not(windows))]
fn start_replacer(_update_exe: &Path, _target_exe: &Path, _expected_version: &str) -> Result<()> {
    bail!("automatic replacement is only supported on Windows");
}

#[cfg(windows)]
fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

#[cfg(windows)]
fn ps_quote_text(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
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
