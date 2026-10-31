//! Proof-of-concept regression tests for the v0.6.0 security backlog
//! (ROADMAP.md, "Code Health & Hardening (v0.6.0 Audit Backlog)").
//!
//! Every test asserts the *secure* behaviour, so on vulnerable code it fails
//! and after the fix it passes. Run them alone with:
//!
//! ```text
//! cargo test --release security_poc
//! ```

use std::path::PathBuf;

use crate::graphics::{self, KittyCommand, Placement, Store, STORE_LIMIT};
use crate::update::{self, AutoUpdateChannel, ReleaseAsset, ReleaseInfo, Version, SUMS_NAME};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("litty-poc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn b64(data: &[u8]) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63]);
        out.push(T[(n >> 12) as usize & 63]);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] } else { b'=' });
        out.push(if c.len() > 2 { T[n as usize & 63] } else { b'=' });
    }
    out
}

fn file_cmd(format: u32, w: u32, h: u32) -> KittyCommand {
    KittyCommand { action: b't', format, medium: b'f', width: w, height: h, ..Default::default() }
}

// ---------------------------------------------------------------------------
// CWE-347 / CWE-321 / CWE-494: auto-update signature verification
// ---------------------------------------------------------------------------

#[test]
fn poc_cwe347_signature_fails_closed_without_verifier() {
    let dir = temp_dir("sig");
    let sums = dir.join(SUMS_NAME);
    std::fs::write(&sums, b"0000  litty\n").unwrap();
    for name in ["SHA256SUMS.txt.minisig", "SHA256SUMS.txt.sig"] {
        let sig = dir.join(name);
        std::fs::write(&sig, b"untrusted comment: forged\nAAAA\n").unwrap();
        assert!(update::verify_signature(&sums, &sig).is_err(), "forged {name} must never verify");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn poc_cwe494_unsigned_release_is_rejected() {
    let dir = temp_dir("unsigned");
    let release = ReleaseInfo {
        tag_name: "v9.9.9".into(),
        version: Version::parse("v9.9.9").unwrap(),
        html_url: String::new(),
        assets: ["litty-test", SUMS_NAME]
            .iter()
            .map(|n| ReleaseAsset { name: n.to_string(), download_url: format!("https://github.com/x/{n}") })
            .collect(),
    };
    let rdir = dir.join("v9.9.9");
    std::fs::create_dir_all(&rdir).unwrap();
    std::fs::write(rdir.join("litty-test"), b"evil binary").unwrap();
    std::fs::write(rdir.join(SUMS_NAME), format!("{}  litty-test\n", update::sha256_digest(b"evil binary"))).unwrap();
    let res = update::download_and_verify_in(&dir, &release, &["litty-test".to_string()]);
    assert!(res.is_err(), "a release without a signature must not be installed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn poc_cwe494_download_url_restricted_to_github() {
    let body = r#"{"tag_name": "v1.2.3", "assets": [
        {"name": "a", "browser_download_url": "https://evil.example/a"},
        {"name": "b", "browser_download_url": "https://github.com.evil.example/b"},
        {"name": "c", "browser_download_url": "https://github.com/Matrix-I/litty-terminal/releases/download/v1.2.3/c"}]}"#;
    let r = update::parse_release_json(body, AutoUpdateChannel::Stable).unwrap();
    let names: Vec<&str> = r.assets.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["c"], "only GitHub-hosted assets may be downloaded");
}

#[test]
fn poc_cwe321_no_runtime_pubkey_override_or_placeholder() {
    let src = include_str!("update.rs");
    for needle in [concat!("var(\"LITTY_", "PUBKEY"), concat!("var(\"LAPLACE_", "PUBKEY"), concat!("RWTPtQ8tZ89sE7J9", "HqZqKj3j")] {
        assert!(!src.contains(needle), "update.rs still contains `{needle}`");
    }
}

// ---------------------------------------------------------------------------
// CWE-200 / CWE-22: Kitty `t=f` file reads
// ---------------------------------------------------------------------------

#[test]
fn poc_cwe200_no_file_existence_oracle() {
    let probe = |p: &str| graphics::decode_kitty(&file_cmd(32, 1, 1), &b64(p.as_bytes())).unwrap_err();
    let exists = probe("/etc/hosts");
    let missing = probe("/etc/litty-poc-does-not-exist");
    assert_eq!(exists, missing, "sensitive paths must fail identically whether or not they exist");
    let hidden_exists = probe(&format!("{}/.ssh", std::env::var("HOME").unwrap()));
    let hidden_missing = probe(&format!("{}/.litty-poc-missing/id", std::env::var("HOME").unwrap()));
    assert_eq!(hidden_exists, hidden_missing);
}

#[cfg(target_os = "macos")]
#[test]
fn poc_cwe22_macos_library_is_blocked() {
    let dir = PathBuf::from(std::env::var("HOME").unwrap()).join("Library/Caches");
    let file = dir.join(format!("litty-poc-{}.rgba", std::process::id()));
    std::fs::write(&file, [1u8, 2, 3, 4]).unwrap();
    let res = graphics::decode_kitty(&file_cmd(32, 1, 1), &b64(file.to_str().unwrap().as_bytes()));
    let _ = std::fs::remove_file(&file);
    assert!(res.is_err(), "~/Library (cookies, keychains, mail) must not be readable through t=f");
}

// ---------------------------------------------------------------------------
// CWE-400 / CWE-770 / CWE-789: image memory caps
// ---------------------------------------------------------------------------

/// A 49-byte Sixel that declares an 8192x8192 raster.
pub(crate) const SIXEL_BOMB: &[u8] = b"\"1;1;8192;8192#0;2;100;0;0#0~";

#[test]
fn poc_cwe789_sixel_bomb_is_capped() {
    let out = graphics::decode_sixel(SIXEL_BOMB, false);
    let bytes = out.as_ref().map_or(0, |(_, _, rgba)| rgba.len());
    assert!(bytes <= STORE_LIMIT, "sixel decoded to {} MiB (> {} MiB)", bytes >> 20, STORE_LIMIT >> 20);
}

#[test]
fn poc_cwe770_raw_rgb_is_capped() {
    // 4300x4300 RGB (52.9 MiB raw) fits in STORE_LIMIT, but the RGBA expansion
    // (70.5 MiB) does not. Compress a 4300x4300 zero stream on the fly without
    // allocating a 53 MiB input buffer in the test harness itself.
    let (w, h) = (4300u32, 4300u32);
    let z = {
        use miniz_oxide::deflate::core::{compress_to_output, create_comp_flags_from_zip_params, CompressorOxide, TDEFLFlush};
        let flags = create_comp_flags_from_zip_params(1, 1, 0);
        let mut comp = CompressorOxide::new(flags);
        let mut out = Vec::new();
        let chunk = [0u8; 65536];
        let mut rem = (w as usize) * (h as usize) * 3;
        while rem > 0 {
            let n = rem.min(chunk.len());
            let flush = if rem == n { TDEFLFlush::Finish } else { TDEFLFlush::None };
            let (_, consumed) = compress_to_output(&mut comp, &chunk[..n], flush, |b| {
                out.extend_from_slice(b);
                true
            });
            rem -= consumed;
        }
        out
    };
    let cmd = KittyCommand { action: b't', format: 24, medium: b'd', compression: b'z', width: w, height: h, ..Default::default() };
    let res = graphics::decode_kitty(&cmd, &b64(&z));
    let bytes = res.as_ref().map_or(0, |(_, _, rgba)| rgba.len());
    assert!(bytes <= STORE_LIMIT, "raw RGB decoded to {} MiB (> {} MiB)", bytes >> 20, STORE_LIMIT >> 20);
}

#[test]
fn poc_cwe400_single_oversized_image_is_evicted() {
    let mut store = Store::default();
    let id = store.insert(7, 1, 1, vec![0u8; STORE_LIMIT + 4]);
    assert!(store.image(id).is_none(), "one image above STORE_LIMIT must not stay resident");
}

#[test]
fn poc_cwe770_placements_are_bounded() {
    let mut store = Store::default();
    let id = store.insert(1, 1, 1, vec![0u8; 4]);
    for i in 0..100_000 {
        store.place(Placement {
            image_id: id,
            placement_id: 0,
            line: i,
            col: 0,
            cols: 1,
            rows: 1,
            src: [0, 0, 1, 1],
            z: 0,
            alt: false,
            fit: false,
        });
    }
    assert!(store.placements.len() <= 10_000, "{} placements kept", store.placements.len());
}

// ---------------------------------------------------------------------------
// CWE-78 / CWE-150: Ctrl+R history insertion
// ---------------------------------------------------------------------------

#[test]
fn poc_cwe78_history_edit_never_executes() {
    let cmd = "echo safe\nrm -rf ~/important";
    for bracketed in [false, true] {
        let p = crate::history_insert_payload(cmd, false, bracketed);
        let body = if bracketed {
            let inner = p.strip_prefix(b"\x05\x15\x1b[200~".as_slice()).expect("bracketed start");
            inner.strip_suffix(b"\x1b[201~".as_slice()).expect("bracketed end").to_vec()
        } else {
            p[2..].to_vec()
        };
        if !bracketed {
            assert!(!body.contains(&b'\n') && !body.contains(&b'\r'), "Tab must not send a line break: {p:?}");
        }
        assert!(!body.contains(&b'\r'), "Tab must not send Enter: {p:?}");
    }
}

#[test]
fn poc_cwe150_history_cannot_break_out_of_bracketed_paste() {
    let p = crate::history_insert_payload("ls\x1b[201~\nid\x07", false, true);
    let esc = p.iter().filter(|&&b| b == 0x1b).count();
    assert_eq!(esc, 2, "only the paste brackets may contain ESC: {p:?}");
    assert!(!p.contains(&0x07));
}

// ---------------------------------------------------------------------------
// CWE-400: clipboard paste helpers
// ---------------------------------------------------------------------------

#[test]
fn poc_cwe400_hung_paste_helpers_are_killed_and_bounded() {
    let dir = temp_dir("paste");
    let pid_file = dir.join("pids");
    let script = dir.join("hung_helper.sh");
    // A helper that records its PID to `pid_file` and then sleeps for 10s.
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho $$ >> '{}'\nexec /bin/sleep 10\n", pid_file.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bin: &'static str = Box::leak(script.to_string_lossy().into_owned().into_boxed_str());
    let cmds: &'static [(&'static str, &'static [&'static str])] = Box::leak(vec![(bin, &[][..])].into_boxed_slice());

    for _ in 0..8 {
        assert!(crate::read_clipboard_with(cmds).is_none());
    }
    std::thread::sleep(std::time::Duration::from_millis(250));

    let pids: Vec<i32> = std::fs::read_to_string(&pid_file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .collect();
    assert!(!pids.is_empty(), "helper must have been spawned at least once");

    let mut alive = 0;
    for &pid in &pids {
        // `kill(pid, 0)` returns 0 if the process still exists.
        if unsafe { libc::kill(pid, 0) } == 0 {
            alive += 1;
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(alive, 0, "{alive} hung clipboard helpers still running after the timeout");
}

// ---------------------------------------------------------------------------
// Performance guard: legitimate images must not get slower.
// Run: cargo test --release perf_legit_images -- --ignored --nocapture
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn perf_legit_images() {
    use std::time::Instant;
    // 800x600 sixel: 100 bands of 800 columns cycling 16 colors.
    let mut sixel = b"\"1;1;800;600".to_vec();
    for band in 0..100 {
        sixel.extend_from_slice(format!("#{}!800~-", band % 16).as_bytes());
    }
    // 1000x1000 raw RGBA kitty transmission and 200 placements.
    let raw = b64(&vec![7u8; 1000 * 1000 * 4]);
    let cmd = KittyCommand { action: b't', format: 32, medium: b'd', width: 1000, height: 1000, ..Default::default() };
    const N: u32 = 30;
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(graphics::decode_sixel(&sixel, false));
    }
    let sixel_ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(graphics::decode_kitty(&cmd, &raw).unwrap());
    }
    let kitty_ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;
    let t = Instant::now();
    for _ in 0..N {
        let mut store = Store::default();
        let id = store.insert(1, 1, 1, vec![0u8; 4]);
        for i in 0..2000 {
            store.place(Placement { image_id: id, placement_id: 0, line: i, col: 0, cols: 1, rows: 1, src: [0, 0, 1, 1], z: 0, alt: false, fit: false });
        }
    }
    let place_ms = t.elapsed().as_secs_f64() * 1e3 / N as f64;
    let t = Instant::now();
    for _ in 0..N * 100 {
        std::hint::black_box(crate::history_insert_payload("git commit -m \"fix: something long enough\"", false, true));
    }
    let hist_us = t.elapsed().as_secs_f64() * 1e6 / (N * 100) as f64;
    println!("PERF sixel_800x600_ms={sixel_ms:.3} kitty_rgba_1000x1000_ms={kitty_ms:.3} place_2000_ms={place_ms:.3} history_payload_us={hist_us:.3}");
}

// ---------------------------------------------------------------------------
// CWE-377: predictable /tmp report
// ---------------------------------------------------------------------------

#[test]
fn poc_cwe377_bench_report_does_not_follow_symlinks() {
    let dir = temp_dir("bench");
    let victim = dir.join("victim");
    let link = dir.join("litty_ram_bench.json");
    std::fs::write(&victim, b"SAFE").unwrap();
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    let _ = crate::bench::write_bench_report(&link, "{\"pwned\":true}");
    let content = std::fs::read(&victim).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(content, b"SAFE", "report write followed a planted symlink");
}

#[test]
fn poc_dead_windows_shell_code_removed() {
    let src = include_str!("main.rs");
    assert!(!src.contains(concat!("\"/c\", ", "\"start\"")), "`cmd /c start` code is still present");
}
