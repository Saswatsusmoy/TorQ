//! Open a torrent stream URL in a real video player.
//!
//! `open`/`xdg-open` would hand an http URL to the default *browser*, which
//! chokes on mkv/webm. torq prefers an actual video player, in priority
//! order, falling back to the platform opener only when none is installed.
//! `config.player` can force a specific player (or `"browser"`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

/// A resolved launch: argv (the URL is appended last) plus a display name.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub name: &'static str,
    pub argv: Vec<String>,
}

/// Candidate players, most preferred first. `bins` are looked up on PATH;
/// `mac_app` is a bundle whose executable lives at
/// `<app>/Contents/MacOS/<exe>` (macOS only).
struct Candidate {
    name: &'static str,
    bins: &'static [&'static str],
    mac_app: Option<(&'static str, &'static str)>,
}

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "VLC",
        bins: &["vlc"],
        mac_app: Some(("VLC.app", "VLC")),
    },
    Candidate {
        name: "IINA",
        bins: &["iina"],
        mac_app: Some(("IINA.app", "iina")),
    },
    Candidate {
        name: "mpv",
        bins: &["mpv"],
        mac_app: Some(("mpv.app", "mpv")),
    },
    Candidate {
        name: "ffplay",
        bins: &["ffplay"],
        mac_app: None,
    },
];

/// Resolve which command plays the URL. Pure (no process spawns, no env
/// reads beyond what is passed in) so the priority/fallback logic is
/// unit-testable with fake PATHs and app dirs.
pub fn resolve(
    override_player: Option<&str>,
    mac: bool,
    path_var: &str,
    app_dirs: &[PathBuf],
) -> Result<Resolved, String> {
    if let Some(over) = override_player {
        if over.eq_ignore_ascii_case("browser") {
            return Ok(opener(mac));
        }
        // A path is used verbatim; anything else is a player name.
        if over.contains('/') {
            return Ok(Resolved {
                name: "configured player",
                argv: vec![over.to_string()],
            });
        }
        for c in CANDIDATES {
            if c.name.eq_ignore_ascii_case(over) {
                return locate(c, mac, path_var, app_dirs)
                    .ok_or_else(|| format!("player '{over}' is not installed"));
            }
        }
        return Err(format!(
            "unknown player '{over}' (try vlc, iina, mpv, ffplay, or browser)"
        ));
    }
    for c in CANDIDATES {
        if let Some(spec) = locate(c, mac, path_var, app_dirs) {
            return Ok(spec);
        }
    }
    Ok(opener(mac))
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

/// Last-resort opener: the platform's URL handler (a browser).
fn opener(mac: bool) -> Resolved {
    if mac {
        Resolved {
            name: "QuickTime Player",
            argv: vec!["open".into(), "-a".into(), "QuickTime Player".into()],
        }
    } else {
        Resolved {
            name: "browser",
            argv: vec!["xdg-open".into()],
        }
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

/// Auto-detected default resolution, cached for the process lifetime.
static DEFAULT: LazyLock<Resolved> = LazyLock::new(|| {
    resolve(None, is_mac(), &path_var(), &app_dirs())
        .expect("auto-resolution always falls back to the opener")
});

/// Launch the stream URL in a player. Returns the player's display name on
/// success. `override_player` (from config) wins when set.
pub fn open_in_player(url: &str, override_player: Option<&str>) -> Result<&'static str, String> {
    let spec = match override_player {
        Some(_) => resolve(override_player, is_mac(), &path_var(), &app_dirs())?,
        None => DEFAULT.clone(),
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

    #[test]
    fn priority_prefers_vlc_over_mpv() {
        let dir = bins_dir(&["vlc", "mpv"]);
        let r = resolve(None, false, &path_for(&dir), &[]).unwrap();
        assert_eq!(r.name, "VLC");
        assert_eq!(r.argv[0], dir.join("vlc").to_string_lossy());
    }

    #[test]
    fn falls_back_to_next_available_player() {
        let dir = bins_dir(&["mpv"]);
        let r = resolve(None, false, &path_for(&dir), &[]).unwrap();
        assert_eq!(r.name, "mpv");
        assert_eq!(r.argv[0], dir.join("mpv").to_string_lossy());
    }

    #[test]
    fn mac_app_bundle_is_found_when_no_binary() {
        let dir = bins_dir(&[]);
        let app = dir.join("VLC.app/Contents/MacOS/VLC");
        std::fs::create_dir_all(app.parent().unwrap()).unwrap();
        std::fs::write(&app, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = resolve(None, true, "", std::slice::from_ref(&dir)).unwrap();
        assert_eq!(r.name, "VLC");
        assert_eq!(r.argv[0], app.to_string_lossy());
    }

    #[test]
    fn no_player_falls_back_to_platform_opener() {
        let dir = bins_dir(&[]);
        let linux = resolve(None, false, &path_for(&dir), &[]).unwrap();
        assert_eq!(linux.name, "browser");
        assert_eq!(linux.argv, vec!["xdg-open".to_string()]);
        let mac = resolve(None, true, "", &[]).unwrap();
        assert_eq!(mac.name, "QuickTime Player");
        assert_eq!(
            mac.argv,
            vec!["open".to_string(), "-a".to_string(), "QuickTime Player".to_string()]
        );
    }

    #[test]
    fn override_browser_forces_opener() {
        let dir = bins_dir(&["vlc"]);
        let r = resolve(Some("browser"), false, &path_for(&dir), &[]).unwrap();
        assert_eq!(r.argv[0], "xdg-open");
        let mac = resolve(Some("Browser"), true, "", &[]).unwrap();
        assert_eq!(mac.name, "QuickTime Player");
    }

    #[test]
    fn override_forces_installed_player_and_errors_otherwise() {
        let dir = bins_dir(&["vlc"]);
        assert!(resolve(Some("mpv"), false, &path_for(&dir), &[]).is_err());
        let mpv_dir = bins_dir(&["vlc", "mpv"]);
        let r = resolve(Some("mpv"), false, &path_for(&mpv_dir), &[]).unwrap();
        assert_eq!(r.name, "mpv");
        assert_eq!(r.argv[0], mpv_dir.join("mpv").to_string_lossy());
    }

    #[test]
    fn override_path_is_used_verbatim() {
        let r = resolve(Some("/opt/fake/player"), false, "", &[]).unwrap();
        assert_eq!(r.argv, vec!["/opt/fake/player".to_string()]);
    }

    #[test]
    fn unknown_override_errors() {
        assert!(resolve(Some("totally-not-a-player"), false, "", &[]).is_err());
    }
}
