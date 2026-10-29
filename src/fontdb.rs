//! Resolve a font family name (e.g. `JetBrainsMono Nerd Font`) to a font file
//! and face index by reading the OpenType `name` table of installed fonts.
//! Only the table directory and `name` table are read, never whole files.

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

static FONT_FILES_CACHE: RwLock<Option<Arc<[PathBuf]>>> = RwLock::new(None);
static FACE_CACHE: RwLock<Option<HashMap<PathBuf, Arc<[FaceName]>>>> = RwLock::new(None);
static FAMILY_CACHE: RwLock<Option<HashMap<String, Option<FaceRef>>>> = RwLock::new(None);

/// Clear all cached font directory scans and parsed metadata, forcing next lookup to re-scan.
#[allow(dead_code)]
pub fn invalidate_font_cache() {
    if let Ok(mut lock) = FONT_FILES_CACHE.write() {
        *lock = None;
    }
    if let Ok(mut lock) = FACE_CACHE.write() {
        *lock = None;
    }
    if let Ok(mut lock) = FAMILY_CACHE.write() {
        *lock = None;
    }
}

fn get_font_files() -> Arc<[PathBuf]> {
    if let Ok(lock) = FONT_FILES_CACHE.read() {
        if let Some(files) = lock.as_ref() {
            return Arc::clone(files);
        }
    }
    let mut files = Vec::new();
    for dir in font_dirs() {
        collect_font_files(&dir, 0, &mut files);
    }
    let arc: Arc<[PathBuf]> = files.into();
    if let Ok(mut lock) = FONT_FILES_CACHE.write() {
        *lock = Some(Arc::clone(&arc));
    }
    arc
}

fn get_cached_faces(path: &Path) -> Arc<[FaceName]> {
    if let Ok(lock) = FACE_CACHE.read() {
        if let Some(cache) = lock.as_ref() {
            if let Some(faces) = cache.get(path) {
                return Arc::clone(faces);
            }
        }
    }
    let parsed: Arc<[FaceName]> = read_faces(path).unwrap_or_default().into();
    if let Ok(mut lock) = FACE_CACHE.write() {
        let map = lock.get_or_insert_with(HashMap::new);
        map.insert(path.to_path_buf(), Arc::clone(&parsed));
    }
    parsed
}

/// A resolved font face: file path and index inside a `.ttc` collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceRef {
    pub path: String,
    pub index: u32,
}

/// Find the best (regular-weight, upright) face of `family`.
/// `family` may also be an absolute path to a font file.
pub fn find_family(family: &str) -> Option<FaceRef> {
    if family.contains('/') {
        return Path::new(family).is_file().then(|| FaceRef { path: family.to_string(), index: 0 });
    }
    if let Ok(lock) = FAMILY_CACHE.read() {
        if let Some(cache) = lock.as_ref() {
            if let Some(res) = cache.get(family) {
                return res.clone();
            }
        }
    }
    let res = resolve_family(family);
    if let Ok(mut lock) = FAMILY_CACHE.write() {
        let map = lock.get_or_insert_with(HashMap::new);
        map.insert(family.to_string(), res.clone());
    }
    res
}

fn resolve_family(family: &str) -> Option<FaceRef> {
    let want = normalize(family);
    if want.is_empty() {
        return None;
    }
    let files = get_font_files();

    // 1. Exact match attempt with full name
    if let Some(face) = search_files(&files, &want) {
        return Some(face);
    }

    // 2. If the user asked for "<Name> Nerd Font" / "<Name> NF", strip the suffix and retry
    // (Ghostty style: base font for text + automatic Nerd Font symbol fallback)
    if let Some(base) = strip_nerd_font_suffix(&want) {
        if let Some(face) = search_files(&files, base) {
            return Some(face);
        }
    }

    // 3. Known aliases on macOS:
    #[cfg(target_os = "macos")]
    {
        if want == "sfmono" || want.starts_with("sfmono") || want == "sfnsmono" || want.starts_with("ligasfmono") {
            for alias in ["ligasfmono", "sfmono", "sfnsmono"] {
                if let Some(face) = search_files(&files, alias) {
                    return Some(face);
                }
            }
        }
    }

    None
}

fn strip_nerd_font_suffix(want: &str) -> Option<&str> {
    for suffix in [
        "nerdfontmono",
        "nerdfontpropo",
        "nerdfont",
        "nfmono",
        "nfpropo",
        "nfm",
        "nfp",
        "nf",
    ] {
        if let Some(base) = want.strip_suffix(suffix) {
            if !base.is_empty() {
                return Some(base);
            }
        }
    }
    None
}

fn search_files(files: &[PathBuf], want: &str) -> Option<FaceRef> {
    let (likely, rest): (Vec<_>, Vec<_>) = files.iter().partition(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).map(normalize).unwrap_or_default();
        stem.starts_with(want) || want.starts_with(&stem)
    });
    best_match(&likely, want).or_else(|| best_match(&rest, want))
}

fn best_match(files: &[&PathBuf], want: &str) -> Option<FaceRef> {
    let mut best: Option<(u8, FaceRef)> = None;
    for &path in files {
        let faces = get_cached_faces(path);
        for face in faces.iter() {
            let fam_norm = normalize(&face.family);
            let ps_norm = normalize(&face.postscript);
            let full_norm = normalize(&face.full_name);

            let exact_match = fam_norm == want || ps_norm == want || full_norm == want;
            let prefix_match = want.len() >= 4 && (fam_norm.starts_with(want) || ps_norm.starts_with(want));

            if !exact_match && !prefix_match {
                continue;
            }

            let score = style_score(&face.subfamily) + if exact_match { 0 } else { 10 };
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, FaceRef { path: path.to_string_lossy().into_owned(), index: face.index }));
                if score == 0 {
                    return best.map(|(_, f)| f);
                }
            }
        }
    }
    best.map(|(_, f)| f)
}

fn style_score(sub: &str) -> u8 {
    let s = sub.to_ascii_lowercase();
    match s.as_str() {
        "regular" => 0,
        "book" | "normal" | "roman" | "text" => 1,
        "medium" => 2,
        _ if s.contains("bold") || s.contains("italic") || s.contains("oblique") => 9,
        _ if s.contains("light") || s.contains("thin") => 5,
        _ => 3,
    }
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Library/Fonts"));
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Applications/Ghostty.app/Contents/Resources/fonts"));
    }
    #[cfg(target_os = "macos")]
    {
        dirs.extend(
            [
                "/Library/Fonts",
                "/System/Library/Fonts",
                "/System/Library/Fonts/Supplemental",
                "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts",
                "/Applications/Ghostty.app/Contents/Resources/fonts",
                "/opt/homebrew/share/fonts",
            ]
            .map(PathBuf::from),
        );
    }
    #[cfg(not(target_os = "macos"))]
    dirs.extend(
        [
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/opt/homebrew/share/fonts",
        ]
        .map(PathBuf::from),
    );
    dirs
}

fn collect_font_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            collect_font_files(&p, depth + 1, out);
        } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc") {
                out.push(p);
            }
        }
    }
}

#[derive(Clone)]
struct FaceName {
    index: u32,
    family: String,
    subfamily: String,
    full_name: String,
    postscript: String,
}

fn read_at(f: &File, off: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    f.read_exact_at(&mut buf, off).ok()?;
    Some(buf)
}

fn be16(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}

fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

struct FaceNameInfo {
    family: String,
    subfamily: String,
    full_name: String,
    postscript: String,
}

fn read_faces(path: &Path) -> Option<Vec<FaceName>> {
    let f = File::open(path).ok()?;
    let head = read_at(&f, 0, 12)?;
    let offsets: Vec<u32> = if &head[0..4] == b"ttcf" {
        let n = be32(&head, 8).min(64) as usize;
        let tbl = read_at(&f, 12, n * 4)?;
        (0..n).map(|i| be32(&tbl, i * 4)).collect()
    } else {
        vec![0]
    };
    let mut faces = Vec::new();
    for (index, off) in offsets.into_iter().enumerate() {
        if let Some(info) = read_name_table(&f, off as u64) {
            faces.push(FaceName {
                index: index as u32,
                family: info.family,
                subfamily: info.subfamily,
                full_name: info.full_name,
                postscript: info.postscript,
            });
        }
    }
    Some(faces)
}

fn read_name_table(f: &File, font_off: u64) -> Option<FaceNameInfo> {
    let hdr = read_at(f, font_off, 12)?;
    let num_tables = be16(&hdr, 4) as usize;
    let dir = read_at(f, font_off + 12, num_tables * 16)?;
    let (mut name_off, mut name_len) = (0u64, 0usize);
    for i in 0..num_tables {
        let r = &dir[i * 16..i * 16 + 16];
        if &r[0..4] == b"name" {
            name_off = be32(r, 8) as u64;
            name_len = be32(r, 12) as usize;
            break;
        }
    }
    if !(6..=1 << 20).contains(&name_len) {
        return None;
    }
    let t = read_at(f, name_off, name_len)?;
    let count = be16(&t, 2) as usize;
    let str_off = be16(&t, 4) as usize;

    // (priority, value) per wanted name: lower priority wins.
    let mut family: Option<(u8, String)> = None;
    let mut sub: Option<(u8, String)> = None;
    let mut full_name: Option<(u8, String)> = None;
    let mut postscript: Option<(u8, String)> = None;
    for i in 0..count {
        let r = 6 + i * 12;
        if r + 12 > t.len() {
            break;
        }
        let (platform, encoding, lang, name_id) = (be16(&t, r), be16(&t, r + 2), be16(&t, r + 4), be16(&t, r + 6));
        let (len, off) = (be16(&t, r + 8) as usize, be16(&t, r + 10) as usize);
        let start = str_off + off;
        if start + len > t.len() {
            continue;
        }
        let raw = &t[start..start + len];
        let english = match platform {
            3 => lang == 0x0409,
            1 => lang == 0,
            _ => true,
        };
        let text = match (platform, encoding) {
            (0, _) | (3, 0 | 1 | 10) => {
                let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&units)
            }
            (1, 0) => raw.iter().map(|&b| b as char).collect(),
            _ => continue,
        };
        // Typographic family/subfamily (16/17) beat legacy ones (1/2).
        let base = if english { 0 } else { 2 };
        match name_id {
            16 => keep(&mut family, base, text),
            1 => keep(&mut family, base + 1, text),
            17 => keep(&mut sub, base, text),
            2 => keep(&mut sub, base + 1, text),
            4 => keep(&mut full_name, base, text),
            6 => keep(&mut postscript, base, text),
            _ => {}
        }
    }
    Some(FaceNameInfo {
        family: family?.1,
        subfamily: sub.map(|s| s.1).unwrap_or_default(),
        full_name: full_name.map(|s| s.1).unwrap_or_default(),
        postscript: postscript.map(|s| s.1).unwrap_or_default(),
    })
}

fn keep(slot: &mut Option<(u8, String)>, prio: u8, text: String) {
    if text.is_empty() {
        return;
    }
    if slot.as_ref().is_none_or(|(p, _)| prio < *p) {
        *slot = Some((prio, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn resolves_system_monospace_family() {
        let face = find_family("Menlo").expect("Menlo should be installed on macOS");
        assert!(face.path.ends_with("Menlo.ttc"), "{face:?}");
        let faces = read_faces(Path::new(&face.path)).unwrap();
        assert_eq!(faces[face.index as usize].subfamily, "Regular");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resolves_sfmono_and_nerd_font_alias() {
        let face = find_family("SFMono Nerd Font").expect("SFMono Nerd Font should resolve on macOS");
        assert!(
            face.path.contains("SF-Mono") || face.path.contains("SFMono") || face.path.contains("SFNSMono"),
            "{face:?}"
        );
        let sf = find_family("SF Mono").expect("SF Mono should resolve on macOS");
        assert!(
            sf.path.contains("SF-Mono") || sf.path.contains("SFMono") || sf.path.contains("SFNSMono"),
            "{sf:?}"
        );
    }

    #[test]
    fn unknown_family_is_none() {
        assert!(find_family("Definitely Not A Real Font 12345").is_none());
    }

    #[test]
    fn cache_memoization_and_invalidation() {
        invalidate_font_cache();
        assert!(find_family("Definitely Not A Real Font 12345").is_none());
        assert!(find_family("Definitely Not A Real Font 12345").is_none());
        invalidate_font_cache();
    }
}
