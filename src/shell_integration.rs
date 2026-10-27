//! Automatic shell integration: the scripts under `src/shell/` are embedded in
//! the binary, written to `~/.cache/laplace/shell-integration/<version>/`, and
//! loaded into the shell without touching the user's dotfiles:
//!
//! - zsh: `ZDOTDIR` points at a `.zshenv` that restores the user's `ZDOTDIR`.
//! - bash: `bash --rcfile <script>`; the script loads the login startup files.
//! - fish, nushell: an `XDG_DATA_DIRS` entry with `fish/vendor_conf.d/` and
//!   `nushell/vendor/autoload/`.
//! - elvish: the same entry puts `elvish/lib/laplace.elv` on the module path;
//!   elvish has no autoload, so `use laplace` in `rc.elv` enables it.
//!
//! The scripts emit OSC 133 prompt marks and OSC 7 directories.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::ShellIntegration;

const ZSHENV: &str = include_str!("shell/zshenv");
const ZSH: &str = include_str!("shell/laplace-integration.zsh");
const BASH: &str = include_str!("shell/laplace.bash");
const FISH: &str = include_str!("shell/laplace-integration.fish");
const NU: &str = include_str!("shell/laplace.nu");
const ELVISH: &str = include_str!("shell/laplace.elv");

/// How to start a shell with the integration loaded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Injection {
    /// Replaces the login-shell `argv` when set (bash needs `--rcfile`).
    pub argv: Option<Vec<String>>,
    /// Variables to set; any inherited variable with the same name is dropped.
    pub env: Vec<(String, String)>,
}

/// Which shell to integrate with: the configured one, or detected from the
/// shell's file name.
fn shell_kind(shell_name: &str, configured: ShellIntegration) -> Option<ShellIntegration> {
    match configured {
        ShellIntegration::None => None,
        ShellIntegration::Detect => match shell_name {
            "zsh" => Some(ShellIntegration::Zsh),
            "bash" => Some(ShellIntegration::Bash),
            "fish" => Some(ShellIntegration::Fish),
            "nu" => Some(ShellIntegration::Nushell),
            "elvish" => Some(ShellIntegration::Elvish),
            _ => None,
        },
        explicit => Some(explicit),
    }
}

/// The injection for `shell_name`, or `None` when the shell is not supported or
/// the scripts could not be written. `get_env` reads the inherited environment.
pub fn prepare(shell_name: &str, configured: ShellIntegration, get_env: impl Fn(&str) -> Option<String>) -> Option<Injection> {
    let kind = shell_kind(shell_name, configured)?;
    let dir = scripts_dir()?;
    Some(injection(kind, dir, get_env))
}

fn injection(kind: ShellIntegration, dir: &Path, get_env: impl Fn(&str) -> Option<String>) -> Injection {
    let dir_str = dir.to_string_lossy().into_owned();
    let mut env = vec![("LAPLACE_SHELL_INTEGRATION_DIR".to_string(), dir_str.clone())];
    let mut argv = None;
    match kind {
        ShellIntegration::Zsh => {
            if let Some(user) = get_env("ZDOTDIR") {
                env.push(("LAPLACE_ZDOTDIR".into(), user));
            }
            env.push(("ZDOTDIR".into(), format!("{dir_str}/zsh")));
        }
        ShellIntegration::Bash => {
            let script = format!("{dir_str}/bash/laplace.bash");
            argv = Some(vec!["bash".to_string(), "--rcfile".to_string(), script]);
            env.push(("LAPLACE_BASH_INJECT".into(), "1".into()));
        }
        ShellIntegration::Fish | ShellIntegration::Nushell | ShellIntegration::Elvish => {
            let data = format!("{dir_str}/data");
            let rest = get_env("XDG_DATA_DIRS").filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            env.push(("XDG_DATA_DIRS".into(), format!("{data}:{rest}")));
            env.push(("LAPLACE_SHELL_INTEGRATION_XDG_DIR".into(), data));
        }
        ShellIntegration::Detect | ShellIntegration::None => {}
    }
    Injection { argv, env }
}

/// Write the scripts once per process into a user-owned cache directory (never
/// a shared `/tmp` path another user could plant files in).
fn scripts_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|c| !c.is_empty())
            .map_or_else(|| PathBuf::from(&home).join(".cache"), PathBuf::from);
        let dir = cache.join("laplace").join("shell-integration").join(env!("CARGO_PKG_VERSION"));
        match write_scripts(&dir) {
            Ok(()) => Some(dir),
            Err(e) => {
                eprintln!("laplace: shell integration disabled, cannot write {}: {e}", dir.display());
                None
            }
        }
    })
    .as_deref()
}

fn write_scripts(dir: &Path) -> std::io::Result<()> {
    let files = [
        ("zsh/.zshenv", ZSHENV),
        ("zsh/laplace-integration.zsh", ZSH),
        ("bash/laplace.bash", BASH),
        ("data/fish/vendor_conf.d/laplace-integration.fish", FISH),
        ("data/nushell/vendor/autoload/laplace.nu", NU),
        ("data/elvish/lib/laplace.elv", ELVISH),
    ];
    for (rel, content) in files {
        let path = dir.join(rel);
        if std::fs::read_to_string(&path).is_ok_and(|old| old == content) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write to a temp file and rename, so a shell starting in another window
        // never reads a half-written script.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(inj: &Injection, key: &str) -> Option<String> {
        inj.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn detects_supported_shells() {
        assert_eq!(shell_kind("zsh", ShellIntegration::Detect), Some(ShellIntegration::Zsh));
        assert_eq!(shell_kind("fish", ShellIntegration::Detect), Some(ShellIntegration::Fish));
        assert_eq!(shell_kind("nu", ShellIntegration::Detect), Some(ShellIntegration::Nushell));
        assert_eq!(shell_kind("elvish", ShellIntegration::Detect), Some(ShellIntegration::Elvish));
        assert_eq!(shell_kind("xonsh", ShellIntegration::Detect), None);
        assert_eq!(shell_kind("zsh", ShellIntegration::None), None);
        assert_eq!(shell_kind("sh", ShellIntegration::Bash), Some(ShellIntegration::Bash));
    }

    #[test]
    fn injections_per_shell() {
        let dir = Path::new("/c/laplace");
        let zsh = injection(ShellIntegration::Zsh, dir, |k| (k == "ZDOTDIR").then(|| "/home/u/.config/zsh".into()));
        assert_eq!(env_of(&zsh, "ZDOTDIR").as_deref(), Some("/c/laplace/zsh"));
        assert_eq!(env_of(&zsh, "LAPLACE_ZDOTDIR").as_deref(), Some("/home/u/.config/zsh"));
        assert_eq!(zsh.argv, None);

        let bash = injection(ShellIntegration::Bash, dir, |_| None);
        let argv = vec!["bash".to_string(), "--rcfile".to_string(), "/c/laplace/bash/laplace.bash".to_string()];
        assert_eq!(bash.argv, Some(argv));
        assert_eq!(env_of(&bash, "LAPLACE_BASH_INJECT").as_deref(), Some("1"));

        let fish = injection(ShellIntegration::Fish, dir, |_| None);
        assert_eq!(env_of(&fish, "XDG_DATA_DIRS").as_deref(), Some("/c/laplace/data:/usr/local/share:/usr/share"));
        let nu = injection(ShellIntegration::Nushell, dir, |k| (k == "XDG_DATA_DIRS").then(|| "/opt/share".into()));
        assert_eq!(env_of(&nu, "XDG_DATA_DIRS").as_deref(), Some("/c/laplace/data:/opt/share"));
        assert_eq!(nu.argv, None);
    }

    #[test]
    fn writes_scripts_atomically_and_idempotently() {
        let dir = std::env::temp_dir().join(format!("laplace-si-test-{}", std::process::id()));
        write_scripts(&dir).unwrap();
        write_scripts(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("zsh/.zshenv")).unwrap(), ZSHENV);
        assert!(dir.join("data/fish/vendor_conf.d/laplace-integration.fish").exists());
        assert!(dir.join("data/nushell/vendor/autoload/laplace.nu").exists());
        assert!(dir.join("data/elvish/lib/laplace.elv").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
