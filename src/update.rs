//! Background and interactive auto-update checks for Litty Terminal.
//!
//! Checks GitHub Releases for new versions, downloads release assets,
//! verifies SHA-256 checksums against `SHA256SUMS.txt`, and replaces
//! the running binary or `.app` bundle safely.

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Update checking mode (Ghostty-compatible `auto-update` configuration).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum AutoUpdateMode {
    /// Periodically check in background and notify when an update is available.
    #[default]
    Check,
    /// Download and verify the update in background, then notify that it is ready.
    /// Installing still waits for the user (Check for Updates… or `litty +update`).
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

const API_LATEST_URL: &str = "https://api.github.com/repos/Matrix-I/litty-terminal/releases/latest";
const API_RELEASES_URL: &str = "https://api.github.com/repos/Matrix-I/litty-terminal/releases";
const WEB_LATEST_URL: &str = "https://github.com/Matrix-I/litty-terminal/releases/latest";
const WEB_TAG_PREFIX: &str = "https://github.com/Matrix-I/litty-terminal/releases/tag/";
const WEB_DOWNLOAD_PREFIX: &str = "https://github.com/Matrix-I/litty-terminal/releases/download/";
const SUMS_NAME: &str = "SHA256SUMS.txt";

/// Path to cache directory `~/.cache/litty`.
fn cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache").join("litty")
    } else {
        std::env::temp_dir().join("litty")
    }
}

/// Directory holding downloaded releases, one subdirectory per tag.
fn updates_dir() -> PathBuf {
    cache_dir().join("updates")
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

/// System binary path resolution with fallback.
fn resolve_system_bin(name: &'static str) -> &'static str {
    match name {
        "curl" => if Path::new("/usr/bin/curl").exists() { "/usr/bin/curl" } else { "curl" },
        "codesign" => if Path::new("/usr/bin/codesign").exists() { "/usr/bin/codesign" } else { "codesign" },
        "touch" => if Path::new("/usr/bin/touch").exists() { "/usr/bin/touch" } else { "touch" },
        _ => name,
    }
}

/// `curl` with the flags every update request shares: fail on HTTP errors, report them on
/// stderr, follow redirects over HTTPS only, and give up on unreachable hosts.
fn curl() -> Command {
    let mut cmd = Command::new(resolve_system_bin("curl"));
    cmd.args([
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--connect-timeout",
        "10",
    ]);
    cmd
}

/// Run a `curl` command and return its stdout, or its stderr as the error.
fn run_curl(cmd: &mut Command, what: &str) -> Result<Vec<u8>, String> {
    run_curl_with_stdin(cmd, what, None)
}

fn run_curl_with_stdin(cmd: &mut Command, what: &str, input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::Stdio;

    let output = if let Some(data) = input {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("cannot run curl: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data);
        }
        child.wait_with_output().map_err(|e| format!("cannot read curl output: {e}"))?
    } else {
        cmd.output().map_err(|e| format!("cannot run curl: {e}"))?
    };

    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr.trim();
    if reason.is_empty() {
        Err(format!("{what} failed (curl exit code {:?})", output.status.code()))
    } else {
        Err(format!("{what} failed: {reason}"))
    }
}

/// Minimal JSON value: just enough to read GitHub release metadata without a dependency.
/// Numbers are validated loosely and dropped, since no release field we read is numeric.
#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num,
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// Nesting limit so a hostile response cannot overflow the stack.
const MAX_JSON_DEPTH: usize = 64;

impl Json {
    fn parse(s: &str) -> Result<Json, String> {
        let mut p = JsonParser { b: s.as_bytes(), i: 0 };
        let value = p.value(0)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(p.err("trailing data"));
        }
        Ok(value)
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct JsonParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl JsonParser<'_> {
    fn err(&self, what: &str) -> String {
        format!("invalid JSON at byte {}: {what}", self.i)
    }

    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(self.err("unexpected token"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members = Vec::new();
                if self.eat(b'}') {
                    return Ok(Json::Obj(members));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    members.push((key, self.value(depth + 1)?));
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(Json::Obj(members));
                    }
                    return Err(self.err("expected ',' or '}'"));
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(Json::Arr(items));
                    }
                    return Err(self.err("expected ',' or ']'"));
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                while matches!(self.b.get(self.i), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.i += 1;
                }
                Ok(Json::Num)
            }
            _ => Err(self.err("unexpected token")),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(self.err("expected string"));
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| self.err("invalid UTF-8")),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or_else(|| self.err("unterminated escape"))?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return Err(self.err("invalid escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err(self.err("control character in string")),
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .b
            .get(self.i..self.i + 4)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .ok_or_else(|| self.err("invalid \\u escape"))?;
        let value = digits.iter().fold(0u32, |acc, &d| acc * 16 + (d as char).to_digit(16).unwrap_or(0));
        self.i += 4;
        Ok(value)
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&hi) {
            if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                return Err(self.err("unpaired surrogate"));
            }
            self.i += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(self.err("unpaired surrogate"));
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(code).ok_or_else(|| self.err("invalid code point"))
    }
}

/// A tag is used as a cache directory name, so allow only plain version-like characters.
fn is_safe_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && !tag.starts_with('.')
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// Read one release object from the GitHub API. Drafts and unparseable tags are skipped.
fn parse_release(v: &Json) -> Option<ReleaseInfo> {
    if v.get("draft") == Some(&Json::Bool(true)) {
        return None;
    }
    let tag_name = v.get("tag_name")?.as_str()?;
    if !is_safe_tag(tag_name) {
        return None;
    }
    let version = Version::parse(tag_name)?;
    let html_url = v
        .get("html_url")
        .and_then(Json::as_str)
        .filter(|u| u.starts_with("https://github.com/"))
        .map_or_else(|| format!("{WEB_TAG_PREFIX}{tag_name}"), str::to_string);
    let assets = match v.get("assets") {
        Some(Json::Arr(list)) => list
            .iter()
            .filter_map(|a| {
                Some(ReleaseAsset {
                    name: a.get("name")?.as_str()?.to_string(),
                    download_url: a
                        .get("browser_download_url")?
                        .as_str()
                        .filter(|u| u.starts_with("https://"))?
                        .to_string(),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    Some(ReleaseInfo { tag_name: tag_name.to_string(), version, html_url, assets })
}

/// Parse an API response: `/releases/latest` (one object) for `stable`, or `/releases`
/// (a list, prereleases included) for `tip`, where the highest version wins.
fn parse_release_json(body: &str, channel: AutoUpdateChannel) -> Result<ReleaseInfo, String> {
    let json = Json::parse(body)?;
    match channel {
        AutoUpdateChannel::Stable => {
            parse_release(&json).ok_or_else(|| "GitHub response contains no usable release".to_string())
        }
        AutoUpdateChannel::Tip => {
            let Json::Arr(list) = &json else {
                return Err("GitHub response is not a release list".to_string());
            };
            // Keep the first of equal versions: the list is newest first, so a final
            // release wins over an older prerelease of the same version.
            list.iter()
                .filter_map(parse_release)
                .filter(|r| !r.assets.is_empty())
                .reduce(|best, r| if r.version > best.version { r } else { best })
                .ok_or_else(|| "GitHub response contains no usable release".to_string())
        }
    }
}

/// Fetch the latest GitHub release metadata.
pub fn fetch_latest_release(channel: AutoUpdateChannel) -> Result<ReleaseInfo, String> {
    match fetch_release_from_api(channel) {
        Ok(release) => Ok(release),
        // The API is rate limited for unauthenticated clients (403); the web redirect is not.
        // It only knows the latest stable release, which is still a valid answer for `tip`.
        Err(api_err) => fetch_latest_release_redirect().map_err(|e| format!("{api_err}\n(fallback: {e})")),
    }
}

fn fetch_release_from_api(channel: AutoUpdateChannel) -> Result<ReleaseInfo, String> {
    let url = match channel {
        AutoUpdateChannel::Stable => API_LATEST_URL,
        AutoUpdateChannel::Tip => API_RELEASES_URL,
    };

    let mut cmd = curl();
    cmd.args([
        "--max-time",
        "30",
        "-H",
        "User-Agent: Litty-Terminal",
        "-H",
        "Accept: application/vnd.github+json",
    ]);
    let mut stdin_config: Option<Vec<u8>> = None;
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        if !token.is_empty() {
            cmd.arg("-K").arg("-");
            stdin_config = Some(format!("header = \"Authorization: Bearer {token}\"\n").into_bytes());
        }
    }
    cmd.arg(url);

    let body = run_curl_with_stdin(&mut cmd, "GitHub API request", stdin_config.as_deref())?;
    let body = String::from_utf8(body).map_err(|_| "GitHub API returned invalid UTF-8".to_string())?;
    parse_release_json(&body, channel)
}

/// Tag name from the final URL of the `/releases/latest` redirect.
fn tag_from_release_url(url: &str) -> Option<&str> {
    url.trim().strip_prefix(WEB_TAG_PREFIX).filter(|tag| is_safe_tag(tag))
}

/// Fallback method: follow `https://github.com/Matrix-I/litty-terminal/releases/latest`,
/// which redirects to the newest tag and does not suffer from GitHub API rate limits.
fn fetch_latest_release_redirect() -> Result<ReleaseInfo, String> {
    let mut cmd = curl();
    cmd.args(["--max-time", "30", "--head", "--output", "/dev/null", "--write-out", "%{url_effective}"]);
    cmd.arg(WEB_LATEST_URL);
    let output = run_curl(&mut cmd, "github.com release lookup")?;
    let final_url = String::from_utf8_lossy(&output);

    let tag_name = tag_from_release_url(&final_url)
        .ok_or_else(|| format!("could not find a release tag in `{}`", final_url.trim()))?
        .to_string();
    let version = Version::parse(&tag_name).ok_or_else(|| format!("invalid version tag `{tag_name}`"))?;

    // Without the API the asset list is unknown: offer this platform's names and let
    // `SHA256SUMS.txt` decide which of them the release really has.
    let assets = platform_asset_candidates()
        .into_iter()
        .chain([SUMS_NAME.to_string()])
        .map(|name| ReleaseAsset { download_url: format!("{WEB_DOWNLOAD_PREFIX}{tag_name}/{name}"), name })
        .collect();

    Ok(ReleaseInfo { html_url: format!("{WEB_TAG_PREFIX}{tag_name}"), tag_name, version, assets })
}

/// Download a file to `dest` using `curl`.
fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let mut cmd = curl();
    // Abort stalled transfers (< 1 KiB/s for a minute) and anything slower than 15 minutes.
    cmd.args(["--max-time", "900", "--speed-limit", "1024", "--speed-time", "60", "--output"]);
    cmd.arg(dest).arg(url);
    run_curl(&mut cmd, &format!("download of {url}")).map(drop)
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

/// Expected SHA-256 of `asset_name` in a `SHA256SUMS.txt` (`<hash>  [*]<name>` lines).
fn expected_hash(sums: &str, asset_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset_name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Checksum validation against the contents of `SHA256SUMS.txt`.
fn verify_checksum(sums: &str, asset_name: &str, downloaded_file: &Path) -> Result<(), String> {
    let expected =
        expected_hash(sums, asset_name).ok_or_else(|| format!("asset `{asset_name}` not found in {SUMS_NAME}"))?;
    let data = fs::read(downloaded_file).map_err(|e| format!("cannot read downloaded file for hashing: {e}"))?;
    let actual = sha256_digest(&data);

    if expected != actual {
        return Err(format!(
            "checksum mismatch for {asset_name}!\nExpected: {expected}\nActual:   {actual}"
        ));
    }
    Ok(())
}

/// Optional cryptographic signature verification (Minisign or GPG).
fn verify_signature(sums_path: &Path, sig_path: &Path) -> Result<(), String> {
    for bin in ["/usr/bin/minisign", "/usr/local/bin/minisign", "/opt/homebrew/bin/minisign"] {
        if Path::new(bin).exists() {
            if let Ok(status) = Command::new(bin)
                .args(["-V", "-m"])
                .arg(sums_path)
                .arg("-x")
                .arg(sig_path)
                .status()
            {
                if status.success() {
                    return Ok(());
                }
            }
        }
    }
    for bin in ["/usr/bin/gpg", "/usr/local/bin/gpg", "/opt/homebrew/bin/gpg"] {
        if Path::new(bin).exists() {
            if let Ok(status) = Command::new(bin)
                .args(["--verify"])
                .arg(sig_path)
                .arg(sums_path)
                .status()
            {
                if status.success() {
                    return Ok(());
                }
            }
        }
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

/// Release asset names for `os`/`arch` (`std::env::consts` naming), most specific first.
/// Releases up to v0.3.0 published unsuffixed names, built for one architecture only
/// (arm64 on macOS, x86_64 on Linux); they are offered only to that architecture.
fn asset_candidates(os: &str, arch: &str, app_bundle: bool) -> Vec<String> {
    let (stem, ext, legacy_arch) = match (os, app_bundle) {
        ("macos", true) => ("Litty-macos", ".zip", "aarch64"),
        ("macos", false) => ("litty-macos", "", "aarch64"),
        _ => ("litty-linux", "", "x86_64"),
    };
    let mut names = vec![format!("{stem}-{arch}{ext}")];
    if arch == legacy_arch {
        names.push(format!("{stem}{ext}"));
    }
    names
}

/// Asset names appropriate for the current platform and installation.
fn platform_asset_candidates() -> Vec<String> {
    #[cfg(target_os = "macos")]
    let app_bundle = find_macos_app_bundle().is_some();
    #[cfg(not(target_os = "macos"))]
    let app_bundle = false;
    asset_candidates(std::env::consts::OS, std::env::consts::ARCH, app_bundle)
}

/// First candidate the release both ships and lists in its checksum file.
fn pick_asset<'a>(release: &'a ReleaseInfo, sums: &str, candidates: &[String]) -> Result<&'a ReleaseAsset, String> {
    candidates
        .iter()
        .find_map(|name| release.assets.iter().find(|a| &a.name == name && expected_hash(sums, name).is_some()))
        .ok_or_else(|| {
            format!(
                "release {} has no build for {}/{} (looked for {})",
                release.tag_name,
                std::env::consts::OS,
                std::env::consts::ARCH,
                candidates.join(", ")
            )
        })
}

fn macho_arch(cpu_type: u32) -> Option<&'static str> {
    match cpu_type {
        0x0100_0007 => Some("x86_64"),
        0x0100_000c => Some("aarch64"),
        _ => None,
    }
}

fn elf_arch(machine: u16) -> Option<&'static str> {
    match machine {
        62 => Some("x86_64"),
        183 => Some("aarch64"),
        _ => None,
    }
}

/// Architectures (`std::env::consts::ARCH` naming) an ELF, Mach-O or universal binary
/// header is built for. Empty when the header is not a recognised executable.
fn binary_arches(h: &[u8]) -> Vec<&'static str> {
    let u16_at = |o: usize, be: bool| {
        h.get(o..o + 2).map(|b| if be { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) })
    };
    let u32_at = |o: usize, be: bool| {
        h.get(o..o + 4).map(|b| {
            let b = [b[0], b[1], b[2], b[3]];
            if be { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
        })
    };
    match h.get(..4) {
        // ELF: e_machine at offset 18, in the byte order given by EI_DATA (2 = big endian).
        Some([0x7f, b'E', b'L', b'F']) => u16_at(18, h.get(5) == Some(&2)).and_then(elf_arch).into_iter().collect(),
        // 64-bit little-endian Mach-O: cputype follows the magic.
        Some([0xcf, 0xfa, 0xed, 0xfe]) => u32_at(4, false).and_then(macho_arch).into_iter().collect(),
        // Universal binary (big endian): nfat_arch, then fat_arch (20 B) or fat_arch_64 (32 B) entries.
        Some([0xca, 0xfe, 0xba, kind @ (0xbe | 0xbf)]) => {
            let stride = if *kind == 0xbe { 20 } else { 32 };
            let count = u32_at(4, true).unwrap_or(0).min(16) as usize;
            (0..count).filter_map(|i| u32_at(8 + i * stride, true)).filter_map(macho_arch).collect()
        }
        _ => Vec::new(),
    }
}

fn ensure_binary_arch(header: &[u8], arch: &str) -> Result<(), String> {
    let found = binary_arches(header);
    if found.contains(&arch) {
        Ok(())
    } else if found.is_empty() {
        Err("downloaded update is not a recognised executable".to_string())
    } else {
        Err(format!("downloaded update is built for {}, but this machine is {arch}", found.join("/")))
    }
}

/// Refuse to install a binary that cannot run on this machine.
fn check_binary_arch(path: &Path) -> Result<(), String> {
    let mut header = Vec::with_capacity(4096);
    fs::File::open(path)
        .and_then(|f| f.take(4096).read_to_end(&mut header))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    ensure_binary_arch(&header, std::env::consts::ARCH)
}

/// Exclusive lock over the updates directory, shared by the app's threads and by
/// `litty +update` in another process (`flock` locks conflict across open files).
/// Released when dropped.
struct UpdateLock(#[allow(dead_code)] fs::File);

impl UpdateLock {
    fn acquire(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".lock"))
            .map_err(|e| format!("cannot open update lock: {e}"))?;
        // SAFETY: flock on a file descriptor we own for the lifetime of `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another Litty update is already in progress".to_string());
        }
        Ok(Self(file))
    }
}

/// Download (or reuse) the release asset for `candidates` into `dir/<tag>/` and verify it.
/// Returns the verified file and its asset name. The caller holds the `UpdateLock`.
fn download_and_verify_in(
    dir: &Path,
    release: &ReleaseInfo,
    candidates: &[String],
) -> Result<(PathBuf, String), String> {
    let release_dir = dir.join(&release.tag_name);
    let sums_path = release_dir.join(SUMS_NAME);

    // Reuse a build that an earlier `auto-update = download` run already fetched.
    if let Ok(sums) = fs::read_to_string(&sums_path) {
        if let Ok(asset) = pick_asset(release, &sums, candidates) {
            let path = release_dir.join(&asset.name);
            if path.is_file() && verify_checksum(&sums, &asset.name, &path).is_ok() {
                return Ok((path, asset.name.clone()));
            }
        }
    }

    let sums_asset = release
        .assets
        .iter()
        .find(|a| a.name == SUMS_NAME)
        .ok_or_else(|| format!("release {} does not contain {SUMS_NAME}", release.tag_name))?;
    fs::create_dir_all(&release_dir).map_err(|e| format!("cannot create {}: {e}", release_dir.display()))?;
    download_file(&sums_asset.download_url, &sums_path)?;
    let sums = fs::read_to_string(&sums_path).map_err(|e| format!("cannot read {SUMS_NAME}: {e}"))?;

    if let Some(sig_asset) = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt.minisig" || a.name == "SHA256SUMS.txt.sig")
    {
        let sig_path = release_dir.join(&sig_asset.name);
        if download_file(&sig_asset.download_url, &sig_path).is_ok() {
            let _ = verify_signature(&sums_path, &sig_path);
        }
    }

    let asset = pick_asset(release, &sums, candidates)?;
    let path = release_dir.join(&asset.name);
    // Only a verified file ever carries the final name, so the reuse check above can trust it.
    let partial = release_dir.join(format!("{}.part", asset.name));
    download_file(&asset.download_url, &partial)?;
    if let Err(e) = verify_checksum(&sums, &asset.name, &partial) {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    fs::rename(&partial, &path).map_err(|e| format!("cannot store verified download: {e}"))?;
    Ok((path, asset.name.clone()))
}

/// Remove everything in the updates directory except the lock and the `keep` release.
fn prune_other_releases(dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == keep || name == ".lock" {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    }
}

/// Download, verify and install `release`, then drop its cached download.
fn install_release(release: &ReleaseInfo) -> Result<(), String> {
    let dir = updates_dir();
    let _lock = UpdateLock::acquire(&dir)?;
    let (file, name) = download_and_verify_in(&dir, release, &platform_asset_candidates())?;
    apply_update(&file, &name)?;
    let _ = fs::remove_dir_all(dir.join(&release.tag_name));
    Ok(())
}

/// Download and verify `release` without installing it (`auto-update = download`).
fn prefetch_release(release: &ReleaseInfo) -> Result<(), String> {
    let dir = updates_dir();
    let _lock = UpdateLock::acquire(&dir)?;
    prune_other_releases(&dir, &release.tag_name);
    download_and_verify_in(&dir, release, &platform_asset_candidates()).map(drop)
}

/// Apply verified update to the current binary or macOS app bundle.
pub fn apply_update(downloaded_file: &Path, asset_name: &str) -> Result<(), String> {
    let current_exe = std::env::current_exe().map_err(|e| format!("cannot determine current executable: {e}"))?;
    let exe_str = current_exe.to_string_lossy();

    // Prevent overwriting local development/debug binaries
    if exe_str.contains("/target/debug/") || exe_str.contains("/target/release/") {
        return Err(format!(
            "Litty is running from a build target ({exe_str}).\nSelf-update is disabled for dev builds. Run `cargo build --release` instead."
        ));
    }

    #[cfg(target_os = "macos")]
    if asset_name.ends_with(".zip") {
        let app_path = find_macos_app_bundle().ok_or("Litty is not running from an .app bundle")?;
        let staging_dir = downloaded_file.with_file_name("staging");
        let _ = fs::remove_dir_all(&staging_dir);
        let _ = fs::create_dir_all(&staging_dir);

        struct StagingCleanup<'a>(&'a Path);
        impl Drop for StagingCleanup<'_> {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(self.0);
            }
        }
        let _guard = StagingCleanup(&staging_dir);

        // Unzip using system ditto for macOS resource fork and symlink fidelity
        let status = Command::new("/usr/bin/ditto")
            .args(["-xk"])
            .arg(downloaded_file)
            .arg(&staging_dir)
            .status()
            .map_err(|e| format!("ditto unpack failed: {e}"))?;

        if !status.success() {
            return Err(format!("failed to extract {asset_name}"));
        }

        let staged_app = staging_dir.join("Litty.app");
        let staged_app = if staged_app.exists() {
            staged_app
        } else {
            staging_dir.join("Laplace.app")
        };
        if !staged_app.exists() {
            return Err("extracted archive did not contain Litty.app".to_string());
        }
        let bin_path = staged_app.join("Contents").join("MacOS").join("litty");
        let bin_path = if bin_path.exists() {
            bin_path
        } else {
            staged_app.join("Contents").join("MacOS").join("laplace")
        };
        check_binary_arch(&bin_path)?;

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
        let _ = Command::new(resolve_system_bin("codesign"))
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&app_path)
            .status();

        let _ = Command::new(resolve_system_bin("touch")).arg(&app_path).status();
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    let _ = asset_name;

    // Standalone binary replacement
    check_binary_arch(downloaded_file)?;
    let temp_dest = current_exe.with_extension("new");
    fs::copy(downloaded_file, &temp_dest).map_err(|e| format!("cannot copy binary: {e}"))?;

    struct FileCleanup<'a>(&'a Path, bool);
    impl Drop for FileCleanup<'_> {
        fn drop(&mut self) {
            if self.1 {
                let _ = fs::remove_file(self.0);
            }
        }
    }
    let mut file_guard = FileCleanup(&temp_dest, true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&temp_dest, fs::Permissions::from_mode(0o755));
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new(resolve_system_bin("codesign"))
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&temp_dest)
            .status();
    }

    fs::rename(&temp_dest, &current_exe).map_err(|e| format!("failed to replace binary {exe_str}: {e}"))?;
    file_guard.1 = false;
    Ok(())
}

/// How to install an update on this platform, for notifications.
fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Choose Litty ▸ Check for Updates… to install."
    } else {
        "Run `litty +update` to install."
    }
}

static CHECKING: AtomicBool = AtomicBool::new(false);
static INSTALLING: AtomicBool = AtomicBool::new(false);

/// Trigger an interactive check for updates (e.g. from macOS menu or keybinding).
/// Runs the network check on a background thread and posts the result back to the main thread.
pub fn start_interactive_check(
    proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>,
    channel: AutoUpdateChannel,
) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let res = fetch_latest_release(channel);
        CHECKING.store(false, Ordering::SeqCst);
        let _ = proxy.send_event(crate::AppEvent::UpdateCheckResult(res));
    });
}

/// Download, verify, and apply update on a background thread, posting the result back to the main thread.
/// Returns `false` (and does nothing) when an install is already running in this process.
pub fn start_install_update(
    release: ReleaseInfo,
    proxy: winit::event_loop::EventLoopProxy<crate::AppEvent>,
) -> bool {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return false;
    }
    std::thread::spawn(move || {
        let res = install_release(&release).map(|()| release.version);
        INSTALLING.store(false, Ordering::SeqCst);
        let _ = proxy.send_event(crate::AppEvent::UpdateInstallResult(res));
    });
    true
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
                    "Litty Update Available",
                    &format!("v{} is available. {}", release.version, install_hint()),
                );
            }
            AutoUpdateMode::Download => match prefetch_release(&release) {
                Ok(()) => crate::show_notification(
                    "Litty Update Ready",
                    &format!("v{} has been downloaded and verified. {}", release.version, install_hint()),
                ),
                Err(e) => eprintln!("litty: background update download failed: {e}"),
            },
            AutoUpdateMode::Off => {}
        }
    });
}

/// CLI command `litty +check-update`.
pub fn cli_check_update() {
    let current = Version::current();
    println!("Litty Terminal — Update Check");
    println!("Current version: v{current}");
    print!("Checking GitHub releases... ");
    match fetch_latest_release(AutoUpdateChannel::Stable) {
        Ok(release) => {
            println!("done.");
            println!("Latest version:  v{}", release.version);
            if release.version > current {
                println!("\n⚡ An update is available: v{current} -> v{}", release.version);
                println!("Release URL: {}", release.html_url);
                println!("\nRun `litty +update` to automatically download and install this update.");
            } else {
                println!("\n✓ You are running the latest version of Litty.");
            }
        }
        Err(e) => {
            println!("failed.");
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// CLI command `litty +update` (supports `--force`).
pub fn cli_update(force: bool) {
    let current = Version::current();
    println!("Litty Terminal — Self-Updater");
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
        println!("✓ Litty is already up to date (v{current}).");
        println!("(To reinstall anyway, run `litty +update --force`)");
        return;
    }

    let dir = updates_dir();
    let _lock = UpdateLock::acquire(&dir).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });

    println!("Downloading the {}/{} build...", std::env::consts::OS, std::env::consts::ARCH);
    let (dest_file, name) = match download_and_verify_in(&dir, &release, &platform_asset_candidates()) {
        Ok(res) => {
            println!("✓ `{}` ready. SHA-256 checksum verified.", res.1);
            res
        }
        Err(e) => {
            eprintln!("Error downloading/verifying update: {e}");
            std::process::exit(1);
        }
    };

    print!("Installing update... ");
    match apply_update(&dest_file, &name) {
        Ok(()) => {
            let _ = fs::remove_dir_all(dir.join(&release.tag_name));
            println!("done!");
            println!("\n🎉 Successfully updated Litty to v{}!", release.version);
            println!("Please restart Litty to use the new version.");
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

    /// Shape of a real `/releases/latest` response: each asset nests an `uploader` object
    /// between `name` and `browser_download_url`, which defeated the old regex.
    const LATEST_JSON: &str = r#"{
        "html_url": "https://github.com/Matrix-I/litty-terminal/releases/tag/v0.4.0",
        "author": {"login": "Matrix-I", "html_url": "https://github.com/Matrix-I", "id": 1},
        "tag_name": "v0.4.0", "draft": false, "prerelease": false, "name": "Litty ⚡ v0.4.0",
        "assets": [
            {"id": 1, "name": "litty-linux", "label": "", "size": 123,
             "uploader": {"login": "Matrix-I", "site_admin": false},
             "browser_download_url": "https://github.com/Matrix-I/litty-terminal/releases/download/v0.4.0/litty-linux"},
            {"id": 2, "name": "SHA256SUMS.txt", "uploader": {"login": "Matrix-I"},
             "browser_download_url": "https://github.com/Matrix-I/litty-terminal/releases/download/v0.4.0/SHA256SUMS.txt"}
        ],
        "body": "Notes with \"quotes\", braces {} and a newline\n"
    }"#;

    fn release(tag: &str, assets: &[&str]) -> ReleaseInfo {
        ReleaseInfo {
            tag_name: tag.to_string(),
            version: Version::parse(tag).unwrap(),
            html_url: format!("{WEB_TAG_PREFIX}{tag}"),
            assets: assets
                .iter()
                .map(|n| ReleaseAsset { name: n.to_string(), download_url: format!("https://invalid.invalid/{n}") })
                .collect(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("litty-update-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    #[test]
    fn test_json_parse_values_and_escapes() {
        let v = Json::parse(r#" {"a": [1, -2.5e3, true, false, null], "s": "x\"\\\/\né😀"} "#).unwrap();
        assert_eq!(
            v.get("a"),
            Some(&Json::Arr(vec![Json::Num, Json::Num, Json::Bool(true), Json::Bool(false), Json::Null]))
        );
        assert_eq!(v.get("s").and_then(Json::as_str), Some("x\"\\/\né😀"));
        assert_eq!(Json::parse("[]"), Ok(Json::Arr(Vec::new())));
        assert_eq!(Json::parse("{}"), Ok(Json::Obj(Vec::new())));
    }

    #[test]
    fn test_json_rejects_malformed_input() {
        for bad in ["", "{", r#"{"a" 1}"#, "[1,]", r#""abc"#, r#""\x""#, r#""\ud83d""#, "{} x", "tru"] {
            assert!(Json::parse(bad).is_err(), "accepted {bad:?}");
        }
        let deep = "[".repeat(MAX_JSON_DEPTH + 2) + &"]".repeat(MAX_JSON_DEPTH + 2);
        assert!(Json::parse(&deep).is_err());
    }

    #[test]
    fn test_parse_latest_release_with_nested_uploader() {
        let r = parse_release_json(LATEST_JSON, AutoUpdateChannel::Stable).unwrap();
        assert_eq!(r.tag_name, "v0.4.0");
        assert_eq!(r.version, Version { major: 0, minor: 4, patch: 0 });
        assert_eq!(r.html_url, "https://github.com/Matrix-I/litty-terminal/releases/tag/v0.4.0");
        let names: Vec<_> = r.assets.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["litty-linux", "SHA256SUMS.txt"]);
        assert!(r.assets[0].download_url.ends_with("/v0.4.0/litty-linux"));
    }

    #[test]
    fn test_parse_tip_picks_highest_version_and_skips_drafts() {
        let asset = r#""assets": [{"name": "a", "browser_download_url": "https://github.com/x/a"}]"#;
        let body = format!(
            r#"[
                {{"tag_name": "v0.9.0", "draft": true, {asset}}},
                {{"tag_name": "v0.5.0", {asset}}},
                {{"tag_name": "v0.5.0-rc1", "prerelease": true, {asset}}},
                {{"tag_name": "v0.6.0-beta.1", "prerelease": true, "assets": []}},
                {{"tag_name": "v0.4.0", {asset}}}
            ]"#
        );
        let r = parse_release_json(&body, AutoUpdateChannel::Tip).unwrap();
        // The draft and the asset-less beta are ignored; the final 0.5.0 beats its rc.
        assert_eq!(r.tag_name, "v0.5.0");
        assert!(parse_release_json("{}", AutoUpdateChannel::Tip).is_err());
        assert!(parse_release_json("[]", AutoUpdateChannel::Tip).is_err());
    }

    #[test]
    fn test_parse_release_rejects_unsafe_tags_and_urls() {
        let body = r#"{"tag_name": "v1.2.3-../../evil", "assets": []}"#;
        assert!(parse_release_json(body, AutoUpdateChannel::Stable).is_err());
        let body = r#"{"tag_name": "v1.2.3", "html_url": "javascript:alert(1)",
            "assets": [{"name": "a", "browser_download_url": "http://insecure/a"}]}"#;
        let r = parse_release_json(body, AutoUpdateChannel::Stable).unwrap();
        assert_eq!(r.html_url, format!("{WEB_TAG_PREFIX}v1.2.3"));
        assert!(r.assets.is_empty());
    }

    #[test]
    fn test_tag_from_release_url() {
        assert_eq!(tag_from_release_url(&format!("{WEB_TAG_PREFIX}v0.3.0\n")), Some("v0.3.0"));
        assert_eq!(tag_from_release_url("https://github.com/Matrix-I/litty-terminal/releases"), None);
        assert_eq!(tag_from_release_url(&format!("{WEB_TAG_PREFIX}..")), None);
        assert_eq!(tag_from_release_url("https://evil.example/releases/tag/v9.9.9"), None);
    }

    #[test]
    fn test_asset_candidates_per_arch() {
        assert_eq!(asset_candidates("macos", "aarch64", true), ["Litty-macos-aarch64.zip", "Litty-macos.zip"]);
        assert_eq!(asset_candidates("macos", "x86_64", true), ["Litty-macos-x86_64.zip"]);
        assert_eq!(asset_candidates("macos", "aarch64", false), ["litty-macos-aarch64", "litty-macos"]);
        assert_eq!(asset_candidates("macos", "x86_64", false), ["litty-macos-x86_64"]);
        assert_eq!(asset_candidates("linux", "x86_64", false), ["litty-linux-x86_64", "litty-linux"]);
        assert_eq!(asset_candidates("linux", "aarch64", false), ["litty-linux-aarch64"]);
    }

    #[test]
    fn test_pick_asset_requires_listing_in_sums() {
        let hash = "a".repeat(64);
        let r = release("v0.4.0", &["litty-linux-x86_64", "litty-linux", SUMS_NAME]);
        let candidates = asset_candidates("linux", "x86_64", false);
        // Only the legacy name is checksummed, so it wins despite being second.
        let sums = format!("{hash}  litty-linux\n");
        assert_eq!(pick_asset(&r, &sums, &candidates).unwrap().name, "litty-linux");
        let sums = format!("{hash}  litty-linux-x86_64\n{hash} *litty-linux\n");
        assert_eq!(pick_asset(&r, &sums, &candidates).unwrap().name, "litty-linux-x86_64");
        let err = pick_asset(&r, &sums, &asset_candidates("linux", "aarch64", false)).unwrap_err();
        assert!(err.contains("litty-linux-aarch64"), "{err}");
    }

    #[test]
    fn test_expected_hash() {
        let hash = "AB".repeat(32);
        let sums = format!("{hash}  Litty.dmg\n{hash} *litty-macos\nnot-a-hash  litty-linux\n\n");
        assert_eq!(expected_hash(&sums, "litty-macos"), Some("ab".repeat(32)));
        assert_eq!(expected_hash(&sums, "litty-linux"), None);
        assert_eq!(expected_hash(&sums, "litty"), None);
    }

    #[test]
    fn test_binary_arches() {
        let mut elf = vec![0u8; 64];
        elf[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert_eq!(binary_arches(&elf), ["x86_64"]);
        elf[18..20].copy_from_slice(&183u16.to_le_bytes());
        assert_eq!(binary_arches(&elf), ["aarch64"]);

        let mut macho = vec![0xcf, 0xfa, 0xed, 0xfe];
        macho.extend_from_slice(&0x0100_000cu32.to_le_bytes());
        assert_eq!(binary_arches(&macho), ["aarch64"]);

        let mut fat = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2];
        for cpu in [0x0100_0007u32, 0x0100_000c] {
            fat.extend_from_slice(&cpu.to_be_bytes());
            fat.extend_from_slice(&[0; 16]);
        }
        assert_eq!(binary_arches(&fat), ["x86_64", "aarch64"]);

        assert!(binary_arches(b"PK\x03\x04zip").is_empty());
        assert!(binary_arches(&[0x7f, b'E']).is_empty());
    }

    #[test]
    fn test_ensure_binary_arch() {
        let mut macho = vec![0xcf, 0xfa, 0xed, 0xfe];
        macho.extend_from_slice(&0x0100_000cu32.to_le_bytes());
        assert!(ensure_binary_arch(&macho, "aarch64").is_ok());
        let err = ensure_binary_arch(&macho, "x86_64").unwrap_err();
        assert!(err.contains("built for aarch64") && err.contains("x86_64"), "{err}");
        assert!(ensure_binary_arch(b"<html>", "x86_64").is_err());
    }

    #[test]
    fn test_update_lock_is_exclusive() {
        let dir = temp_dir("lock");
        let first = UpdateLock::acquire(&dir).unwrap();
        assert!(UpdateLock::acquire(&dir).is_err());
        drop(first);
        assert!(UpdateLock::acquire(&dir).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_download_reuses_verified_cache_without_network() {
        let dir = temp_dir("cache");
        let r = release("v9.9.9", &["litty-test", SUMS_NAME]);
        let release_dir = dir.join(&r.tag_name);
        fs::create_dir_all(&release_dir).unwrap();
        fs::write(release_dir.join("litty-test"), b"binary").unwrap();
        fs::write(release_dir.join(SUMS_NAME), format!("{}  litty-test\n", sha256_digest(b"binary"))).unwrap();

        // Asset URLs are unreachable, so success proves the cached copy was used.
        let (path, name) = download_and_verify_in(&dir, &r, &["litty-test".to_string()]).unwrap();
        assert_eq!(name, "litty-test");
        assert_eq!(path, release_dir.join("litty-test"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_prune_other_releases_keeps_lock_and_current() {
        let dir = temp_dir("prune");
        for sub in ["v0.3.0", "v0.4.0", "staging"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::write(dir.join(".lock"), b"").unwrap();
        fs::write(dir.join("litty-macos"), b"old layout").unwrap();
        prune_other_releases(&dir, "v0.4.0");
        let mut left: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        left.sort();
        assert_eq!(left, [".lock", "v0.4.0"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
