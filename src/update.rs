//! Background and interactive auto-update checks for Laplace Terminal.
//!
//! Checks GitHub Releases for new versions, downloads release assets,
//! verifies SHA-256 checksums against `SHA256SUMS.txt`, and replaces
//! the running binary or `.app` bundle safely.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

/// Update checking mode (Ghostty-compatible `auto-update` configuration).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum AutoUpdateMode {
    /// Periodically check in background and notify when an update is available.
    #[default]
    Check,
    /// Download update in background and notify when ready to install.
    Download,
    /// Do not check for updates automatically.
    Off,
}

/// Channel to track for updates.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum AutoUpdateChannel {
    #[default]
    Stable,
    Tip,
}

/// Parsed semantic version `major.minor.patch`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim().trim_start_matches(['v', 'V']);
        // Handle prerelease tags like "0.3.1-beta.1"
        let core = trimmed.split_once('-').map_or(trimmed, |(c, _)| c);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some(Self { major, minor, patch })
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self { major: 0, minor: 0, patch: 0 })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Release asset metadata.
#[derive(Clone, Debug)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

/// GitHub release information.
#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub version: Version,
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
}

/// Path to cache directory `~/.cache/laplace`.
fn cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache").join("laplace")
    } else {
        std::env::temp_dir().join("laplace")
    }
}

/// Path to timestamp file recording the last update check.
fn last_check_file() -> PathBuf {
    cache_dir().join("last_update_check")
}

/// Check if at least 24 hours have elapsed since the last background update check.
pub fn is_check_due() -> bool {
    let path = last_check_file();
    let Ok(content) = fs::read_to_string(&path) else {
        return true;
    };
    let Ok(ts) = content.trim().parse::<u64>() else {
        return true;
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // 24 hours = 86400 seconds
    now.saturating_sub(ts) >= 86400
}

/// Record current timestamp as the last update check time.
pub fn mark_checked() {
    let dir = cache_dir();
    let _ = fs::create_dir_all(&dir);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = fs::write(last_check_file(), now.to_string());
}

/// Fetch the latest GitHub release metadata.
pub fn fetch_latest_release(channel: AutoUpdateChannel) -> Result<ReleaseInfo, String> {
    let url = match channel {
        AutoUpdateChannel::Stable => "https://api.github.com/repos/Matrix-I/laplace-terminal/releases/latest",
        AutoUpdateChannel::Tip => "https://api.github.com/repos/Matrix-I/laplace-terminal/releases",
    };

    let mut cmd = Command::new("curl");
    cmd.args([
        "-s",
        "-L",
        "-f",
        "-H",
        "User-Agent: Laplace-Terminal",
        "-H",
        "Accept: application/vnd.github.v3+json",
    ]);

    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        if !token.is_empty() {
            cmd.arg("-H").arg(format!("Authorization: Bearer {token}"));
        }
    }

    let output = cmd.arg(url).output().map_err(|e| format!("curl execution failed: {e}"))?;

    if output.status.success() {
        if let Ok(body) = String::from_utf8(output.stdout) {
            let json_block = &body;

            let tag_re = Regex::new(r#""tag_name"\s*:\s*"([^"]+)""#).map_err(|e| e.to_string())?;
            let html_re = Regex::new(r#""html_url"\s*:\s*"(https://github\.com/[^"]+)""#).map_err(|e| e.to_string())?;
            let asset_re = Regex::new(
                r#"\{[^{}]*?"name"\s*:\s*"([^"]+)"[^{}]*?"browser_download_url"\s*:\s*"([^"]+)"[^{}]*?\}"#,
            )
            .map_err(|e| e.to_string())?;

            if let Some(tag_name) = tag_re.captures(json_block).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()) {
                if let Some(version) = Version::parse(&tag_name) {
                    let html_url = html_re
                        .captures(json_block)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_else(|| format!("https://github.com/Matrix-I/laplace-terminal/releases/tag/{tag_name}"));

                    let mut assets = Vec::new();
                    for cap in asset_re.captures_iter(json_block) {
                        if let (Some(name), Some(url)) = (cap.get(1), cap.get(2)) {
                            assets.push(ReleaseAsset {
                                name: name.as_str().to_string(),
                                download_url: url.as_str().to_string(),
                            });
                        }
                    }
                    if !assets.is_empty() {
                        return Ok(ReleaseInfo { tag_name, version, html_url, assets });
                    }
                }
            }
        }
    }

    // Fallback: If GitHub API fails (e.g. rate limit 403 on unauthenticated requests),
    // query the web redirect of /releases/latest which has no rate limit.
    fetch_latest_release_redirect()
}

/// Fallback method: query `https://github.com/Matrix-I/laplace-terminal/releases/latest`
/// HTTP redirect URL which does not suffer from GitHub API rate limits.
fn fetch_latest_release_redirect() -> Result<ReleaseInfo, String> {
    let output = Command::new("curl")
        .args([
            "-s",
            "-I",
            "-o",
            "/dev/null",
            "-w",
            "%{redirect_url}",
            "https://github.com/Matrix-I/laplace-terminal/releases/latest",
        ])
        .output()
        .map_err(|e| format!("curl redirect check failed: {e}"))?;

    if !output.status.success() {
        return Err("failed to connect to github.com".to_string());
    }

    let redirect_url = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    let redirect_url = redirect_url.trim();
    if redirect_url.is_empty() {
        return Err("no redirect received from github.com/Matrix-I/laplace-terminal/releases/latest".to_string());
    }

    // URL format: https://github.com/Matrix-I/laplace-terminal/releases/tag/vX.Y.Z
    let tag_name = redirect_url
        .rsplit('/')
        .next()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("could not parse tag name from redirect `{redirect_url}`"))?
        .to_string();

    let version = Version::parse(&tag_name).ok_or_else(|| format!("invalid version tag `{tag_name}`"))?;
    let html_url = redirect_url.to_string();

    let base_download = format!("https://github.com/Matrix-I/laplace-terminal/releases/download/{tag_name}");
    let known_asset_names = [
        "Laplace-macos.zip",
        "Laplace.dmg",
        "laplace-macos",
        "laplace-linux",
        "laplace-linux-x86_64.tar.gz",
        "SHA256SUMS.txt",
    ];

    let assets = known_asset_names
        .iter()
        .map(|&name| ReleaseAsset {
            name: name.to_string(),
            download_url: format!("{base_download}/{name}"),
        })
        .collect();

    Ok(ReleaseInfo { tag_name, version, html_url, assets })
}

/// Download a file to `dest` using `curl`.
fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let status = Command::new("curl")
        .args(["-s", "-L", "-f", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl download failed: {e}"))?;

    if !status.success() {
        return Err(format!("download of {url} failed with exit code {:?}", status.code()));
    }
    Ok(())
}

/// Standalone, pure-Rust SHA-256 implementation (FIPS 180-4).
/// Completely allocation-free block compression; zero external dependencies.
#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn sha256_digest(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, part) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([part[0], part[1], part[2], part[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    let mut res = String::with_capacity(64);
    for val in h {
        res.push_str(&format!("{val:08x}"));
    }
    res
}

/// Checksum validation from `SHA256SUMS.txt`.
fn verify_checksum(sums_file: &Path, asset_name: &str, downloaded_file: &Path) -> Result<(), String> {
    let content = fs::read_to_string(sums_file).map_err(|e| format!("cannot read checksum file: {e}"))?;
    let mut expected_hash = None;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1].trim_start_matches('*') == asset_name {
            expected_hash = Some(parts[0].to_lowercase());
            break;
        }
    }

    let expected = expected_hash.ok_or_else(|| format!("asset `{asset_name}` not found in SHA256SUMS.txt"))?;
    let data = fs::read(downloaded_file).map_err(|e| format!("cannot read downloaded file for hashing: {e}"))?;
    let actual = sha256_digest(&data);

    if expected != actual {
        return Err(format!(
            "checksum mismatch for {asset_name}!\nExpected: {expected}\nActual:   {actual}"
        ));
    }
    Ok(())
}

/// Detect if current executable is running inside an `.app` bundle on macOS.
#[cfg(target_os = "macos")]
fn find_macos_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut current = exe.parent()?;
    while let Some(parent) = current.parent() {
        if current.extension().is_some_and(|ext| ext == "app") {
            return Some(current.to_path_buf());
        }
        current = parent;
    }
    None
}

/// Select the release asset name appropriate for the current platform and installation.
fn target_asset_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        if find_macos_app_bundle().is_some() {
            "Laplace-macos.zip"
        } else {
            "laplace-macos"
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        "laplace-linux"
    }
}

/// Download asset and verify SHA-256. Returns path to verified downloaded file.
pub fn download_and_verify(release: &ReleaseInfo) -> Result<(PathBuf, &'static str), String> {
    let asset_name = target_asset_name();
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| format!("release {} does not contain asset `{asset_name}`", release.tag_name))?;

    let checksum_asset = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .ok_or_else(|| format!("release {} does not contain SHA256SUMS.txt", release.tag_name))?;

    let updates_dir = cache_dir().join("updates");
    let dest_file = updates_dir.join(asset_name);
    let sums_file = updates_dir.join("SHA256SUMS.txt");

    download_file(&checksum_asset.download_url, &sums_file)?;
    download_file(&asset.download_url, &dest_file)?;

    verify_checksum(&sums_file, asset_name, &dest_file)?;

    Ok((dest_file, asset_name))
}

/// Apply verified update to the current binary or macOS app bundle.
pub fn apply_update(downloaded_file: &Path, _asset_name: &str) -> Result<(), String> {
    let current_exe = std::env::current_exe().map_err(|e| format!("cannot determine current executable: {e}"))?;
    let exe_str = current_exe.to_string_lossy();

    // Prevent overwriting local development/debug binaries
    if exe_str.contains("/target/debug/") || exe_str.contains("/target/release/") {
        return Err(format!(
            "Laplace is running from a build target ({exe_str}).\nSelf-update is disabled for dev builds. Run `cargo build --release` instead."
        ));
    }

    #[cfg(target_os = "macos")]
    if _asset_name == "Laplace-macos.zip" {
        if let Some(app_path) = find_macos_app_bundle() {
            let staging_dir = cache_dir().join("updates").join("staging");
            let _ = fs::remove_dir_all(&staging_dir);
            let _ = fs::create_dir_all(&staging_dir);

            // Unzip using system ditto for macOS resource fork and symlink fidelity
            let status = Command::new("/usr/bin/ditto")
                .args(["-xk"])
                .arg(downloaded_file)
                .arg(&staging_dir)
                .status()
                .map_err(|e| format!("ditto unpack failed: {e}"))?;

            if !status.success() {
                return Err("failed to extract Laplace-macos.zip".to_string());
            }

            let staged_app = staging_dir.join("Laplace.app");
            if !staged_app.exists() {
                return Err("extracted archive did not contain Laplace.app".to_string());
            }

            // Sync updated contents to app_path
            let status = Command::new("/usr/bin/ditto")
                .arg(&staged_app)
                .arg(&app_path)
                .status()
                .map_err(|e| format!("failed to overwrite app bundle: {e}"))?;

            if !status.success() {
                return Err("failed to copy update into application directory (check permissions)".to_string());
            }

            // Ad-hoc sign so Gatekeeper does not block the updated bundle
            let _ = Command::new("codesign")
                .args(["--force", "--deep", "--sign", "-"])
                .arg(&app_path)
                .status();

            let _ = Command::new("touch").arg(&app_path).status();
            let _ = fs::remove_dir_all(&staging_dir);
            let _ = fs::remove_file(downloaded_file);
            return Ok(());
        }
    }

    // Standalone binary replacement
    let temp_dest = current_exe.with_extension("new");
    fs::copy(downloaded_file, &temp_dest).map_err(|e| format!("cannot copy binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&temp_dest, fs::Permissions::from_mode(0o755));
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&temp_dest)
            .status();
    }

    fs::rename(&temp_dest, &current_exe).map_err(|e| format!("failed to replace binary {exe_str}: {e}"))?;
    let _ = fs::remove_file(downloaded_file);
    Ok(())
}

static CHECKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Trigger an interactive check for updates (e.g. from macOS menu or keybinding).
/// Runs the network check on a background thread and posts the result back to the main thread.
pub fn start_interactive_check(proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>) {
    if CHECKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let res = fetch_latest_release(AutoUpdateChannel::Stable);
        CHECKING.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = proxy.send_event(crate::AppEvent::UpdateCheckResult(res));
    });
}

/// Download, verify, and apply update on a background thread, posting the result back to the main thread.
pub fn start_install_update(
    release: ReleaseInfo,
    proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>,
) {
    std::thread::spawn(move || {
        let res = (|| -> Result<Version, String> {
            let (file, name) = download_and_verify(&release)?;
            apply_update(&file, name)?;
            Ok(release.version)
        })();
        let _ = proxy.send_event(crate::AppEvent::UpdateInstallResult(res));
    });
}

/// Startup check run in a background thread if `auto_update != Off`.
pub fn check_on_startup(mode: AutoUpdateMode, channel: AutoUpdateChannel) {
    if mode == AutoUpdateMode::Off || !is_check_due() {
        return;
    }

    std::thread::spawn(move || {
        let current = Version::current();
        let Ok(release) = fetch_latest_release(channel) else {
            return;
        };
        mark_checked();

        if release.version <= current {
            return;
        }

        match mode {
            AutoUpdateMode::Check => {
                crate::show_notification(
                    "Laplace Update Available",
                    &format!("v{} is available. Choose Laplace ▸ Check for Updates… to install.", release.version),
                );
            }
            AutoUpdateMode::Download => {
                if let Ok((file, name)) = download_and_verify(&release) {
                    if apply_update(&file, name).is_ok() {
                        crate::show_notification(
                            "Laplace Updated",
                            &format!("v{} has been installed. Restart Laplace to apply.", release.version),
                        );
                    }
                }
            }
            AutoUpdateMode::Off => {}
        }
    });
}

/// CLI command `laplace +check-update`.
pub fn cli_check_update() {
    let current = Version::current();
    println!("Laplace Terminal — Update Check");
    println!("Current version: v{current}");
    print!("Checking GitHub releases... ");
    match fetch_latest_release(AutoUpdateChannel::Stable) {
        Ok(release) => {
            println!("done.");
            println!("Latest version:  v{}", release.version);
            if release.version > current {
                println!("\n⚡ An update is available: v{current} -> v{}", release.version);
                println!("Release URL: {}", release.html_url);
                println!("\nRun `laplace +update` to automatically download and install this update.");
            } else {
                println!("\n✓ You are running the latest version of Laplace.");
            }
        }
        Err(e) => {
            println!("failed.");
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// CLI command `laplace +update` (supports `--force`).
pub fn cli_update(force: bool) {
    let current = Version::current();
    println!("Laplace Terminal — Self-Updater");
    println!("Current version: v{current}");
    print!("Checking for new releases... ");
    let release = match fetch_latest_release(AutoUpdateChannel::Stable) {
        Ok(r) => {
            println!("done (found v{}).", r.version);
            r
        }
        Err(e) => {
            println!("failed.");
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    if release.version <= current && !force {
        println!("✓ Laplace is already up to date (v{current}).");
        println!("(To reinstall anyway, run `laplace +update --force`)");
        return;
    }

    let asset_name = target_asset_name();
    println!("Downloading release asset `{asset_name}`...");
    let (dest_file, name) = match download_and_verify(&release) {
        Ok(res) => {
            println!("✓ Download complete. SHA-256 checksum verified.");
            res
        }
        Err(e) => {
            eprintln!("Error downloading/verifying update: {e}");
            std::process::exit(1);
        }
    };

    print!("Installing update... ");
    match apply_update(&dest_file, name) {
        Ok(()) => {
            println!("done!");
            println!("\n🎉 Successfully updated Laplace to v{}!", release.version);
            println!("Please restart Laplace to use the new version.");
        }
        Err(e) => {
            println!("failed.");
            eprintln!("Error applying update: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_parse() {
        assert_eq!(Version::parse("v0.3.0"), Some(Version { major: 0, minor: 3, patch: 0 }));
        assert_eq!(Version::parse("0.3.1"), Some(Version { major: 0, minor: 3, patch: 1 }));
        assert_eq!(Version::parse("v1.0.0-rc1"), Some(Version { major: 1, minor: 0, patch: 0 }));
        assert!(Version::parse("invalid").is_none());
    }

    #[test]
    fn test_version_comparison() {
        let v030 = Version { major: 0, minor: 3, patch: 0 };
        let v031 = Version { major: 0, minor: 3, patch: 1 };
        let v100 = Version { major: 1, minor: 0, patch: 0 };
        assert!(v031 > v030);
        assert!(v100 > v031);
        assert_eq!(v030, Version { major: 0, minor: 3, patch: 0 });
    }

    #[test]
    fn test_sha256_nist_vectors() {
        // Standard NIST test vectors
        assert_eq!(
            sha256_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
