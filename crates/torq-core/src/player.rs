//! Open a torrent stream URL in VLC.
//!
//! VLC is the only supported player for now: it is the one player whose
//! HTTP streaming behavior is known-good against torq's mid-download
//! ranges. `config.player` can still point at a specific VLC binary (or
//! `"vlc"`), but non-VLC players are rejected.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

/// A resolved launch: argv (the URL is appended last) plus a display name.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub name: &'static str,
    pub argv: Vec<String>,
}

/// The one supported player. `bins` are looked up on PATH; `mac_app` is the
/// macOS bundle whose executable lives at `<app>/Contents/MacOS/<exe>`.
struct Candidate {
    name: &'static str,
    bins: &'static [&'static str],
    mac_app: Option<(&'static str, &'static str)>,
}

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "VLC",
    bins: &["vlc"],
    mac_app: Some(("VLC.app", "VLC")),
}];

/// Resolve which command plays the URL. Pure (no process spawns, no env
/// reads beyond what is passed in) so the resolution logic is unit-testable
/// with fake PATHs and app dirs.
pub fn resolve(
    override_player: Option<&str>,
    mac: bool,
    path_var: &str,
    app_dirs: &[PathBuf],
) -> Result<Resolved, String> {
    if let Some(over) = override_player {
        // A path is used verbatim (it is expected to be a VLC binary).
        if over.contains('/') {
            return Ok(Resolved {
                name: "VLC",
                argv: vec![over.to_string()],
            });
        }
        if over.eq_ignore_ascii_case("vlc") {
            return locate(&CANDIDATES[0], mac, path_var, app_dirs)
                .ok_or_else(|| "VLC is not installed".to_string());
        }
        return Err(format!(
            "only VLC is supported for playback (set player = \"vlc\" or a path to the vlc binary); got '{over}'"
        ));
    }
    locate(&CANDIDATES[0], mac, path_var, app_dirs).ok_or_else(|| {
        "VLC is not installed — torq requires VLC to play streams \
         (macOS: brew install --cask vlc; Linux: your package manager's vlc)"
            .to_string()
    })
}

fn locate(c: &Candidate, mac: bool, path_var: &str, app_dirs: &[PathBuf]) -> Option<Resolved> {
    if let Some(bin) = find_on_path(c.bins, path_var) {
        return Some(Resolved {
            name: c.name,
            argv: vec![bin],
        });
    }
    if mac
        && let Some((app, exe)) = c.mac_app
    {
        for dir in app_dirs {
            let p = dir.join(app).join("Contents").join("MacOS").join(exe);
            if is_executable(&p) {
                return Some(Resolved {
                    name: c.name,
                    argv: vec![p.to_string_lossy().into_owned()],
                });
            }
        }
    }
    None
}

fn find_on_path(bins: &[&str], path_var: &str) -> Option<String> {
    for dir in std::env::split_paths(path_var) {
        for bin in bins {
            let p = dir.join(bin);
            if is_executable(&p) {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file()
            && p.metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

fn path_var() -> String {
    std::env::var("PATH").unwrap_or_default()
}

fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Applications"));
    }
    dirs
}

/// Auto-detected VLC resolution, cached for the process lifetime.
static DEFAULT: LazyLock<Result<Resolved, String>> = LazyLock::new(|| {
    resolve(None, is_mac(), &path_var(), &app_dirs())
});

/// Launch the stream URL in VLC. Returns "VLC" on success.
/// `override_player` (from config) wins when set.
pub fn open_in_player(url: &str, override_player: Option<&str>) -> Result<&'static str, String> {
    let spec = match override_player {
        Some(_) => resolve(override_player, is_mac(), &path_var(), &app_dirs())?,
        None => DEFAULT.clone()?,
    };
    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]);
    cmd.arg(url);
    cmd.spawn()
        .map_err(|e| format!("failed to start {}: {e}", spec.name))?;
    Ok(spec.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a unique temp dir with executable `bins`; caller owns cleanup.
    fn bins_dir(bins: &[&str]) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "torq-player-test-{}-{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for b in bins {
            let p = dir.join(b);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        dir
    }

    fn path_for(dir: &Path) -> String {
        std::env::join_paths([dir]).unwrap().into_string().unwrap()
    }

    fn mac_app_dir(dir: &Path) -> PathBuf {
        let app = dir.join("VLC.app/Contents/MacOS/VLC");
        std::fs::create_dir_all(app.parent().unwrap()).unwrap();
        std::fs::write(&app, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        app
    }

    #[test]
    fn detects_vlc_from_path() {
        let dir = bins_dir(&["vlc", "mpv"]);
        let r = resolve(None, false, &path_for(&dir), &[]).unwrap();
        assert_eq!(r.name, "VLC");
        assert_eq!(r.argv[0], dir.join("vlc").to_string_lossy());
    }

    #[test]
    fn mac_app_bundle_is_found_when_no_binary() {
        let dir = bins_dir(&[]);
        let app = mac_app_dir(&dir);
        let r = resolve(None, true, "", std::slice::from_ref(&dir)).unwrap();
        assert_eq!(r.name, "VLC");
        assert_eq!(r.argv[0], app.to_string_lossy());
    }

    #[test]
    fn missing_vlc_errors_with_clear_message() {
        let dir = bins_dir(&["mpv"]);
        let err = resolve(None, false, &path_for(&dir), &[]).unwrap_err();
        assert!(err.contains("VLC is not installed"), "{err}");
        assert!(err.contains("requires VLC"), "{err}");
    }

    #[test]
    fn override_vlc_works_and_others_are_rejected() {
        let dir = bins_dir(&["vlc"]);
        let r = resolve(Some("vlc"), false, &path_for(&dir), &[]).unwrap();
        assert_eq!(r.name, "VLC");
        assert!(resolve(Some("iina"), false, &path_for(&dir), &[]).is_err());
        assert!(resolve(Some("mpv"), false, &path_for(&dir), &[]).is_err());
        assert!(resolve(Some("browser"), false, &path_for(&dir), &[]).is_err());
        // A named player that is simply missing also errors.
        assert!(resolve(Some("vlc"), false, "", &[]).is_err());
    }

    #[test]
    fn override_path_is_used_verbatim() {
        let r = resolve(Some("/opt/vlc/vlc"), false, "", &[]).unwrap();
        assert_eq!(r.argv, vec!["/opt/vlc/vlc".to_string()]);
    }

    #[test]
    fn unknown_override_errors() {
        assert!(resolve(Some("totally-not-a-player"), false, "", &[]).is_err());
    }
}
