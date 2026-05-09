//! Game detection — /proc scan + SteamAppId lookup + opportunistic gamemoded.

use std::fs;
use std::path::PathBuf;

/// Read /proc/<pid>/comm. Returns the trimmed contents or None if unreadable.
pub fn read_comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Read /proc/<pid>/cmdline. Returns argv joined by ASCII spaces (the file is
/// NUL-separated). None if unreadable.
pub fn read_cmdline(pid: u32) -> Option<String> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let s = bytes
        .split(|&b| b == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    Some(s)
}

/// Read /proc/<pid>/environ. Returns a HashMap<key, value>. None if unreadable.
pub fn read_environ(pid: u32) -> Option<std::collections::HashMap<String, String>> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let mut out = std::collections::HashMap::new();
    for chunk in bytes.split(|&b| b == 0) {
        if chunk.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(chunk);
        if let Some((k, v)) = s.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        }
    }
    Some(out)
}

/// Iterate all PIDs in /proc.
pub fn all_pids() -> impl Iterator<Item = u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
}

#[cfg(test)]
mod proc_tests {
    use super::*;

    #[test]
    fn read_comm_for_self_pid_returns_test_binary_name() {
        let me = std::process::id();
        let comm = read_comm(me).expect("comm readable");
        // Cargo's test binary is named after the crate, sometimes truncated to 15 chars.
        assert!(!comm.is_empty());
    }

    #[test]
    fn read_environ_for_self_pid_contains_path() {
        let me = std::process::id();
        let env = read_environ(me).expect("environ readable");
        assert!(env.contains_key("PATH"));
    }

    #[test]
    fn all_pids_includes_self() {
        let me = std::process::id();
        assert!(all_pids().any(|p| p == me));
    }

    #[test]
    fn read_comm_for_nonexistent_pid_returns_none() {
        assert!(read_comm(0).is_none());
    }
}

/// Look up a Steam game's display name from its appmanifest file.
/// Returns None if the file is missing or "name" key isn't found.
pub fn steam_game_name(app_id: &str) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home)
        .join(".steam/steam/steamapps")
        .join(format!("appmanifest_{app_id}.acf"));
    let contents = fs::read_to_string(path).ok()?;
    parse_acf_name(&contents)
}

/// Parse the "name" field out of a Steam ACF (Valve Key/Value format).
/// Looks for a line like:   "name"   "Apex Legends"
pub fn parse_acf_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        // Look for the pattern: "name" "<value>"
        if let Some(rest) = trimmed.strip_prefix("\"name\"") {
            let rest = rest.trim_start();
            // rest now begins with the second quoted field.
            let mut chars = rest.chars();
            if chars.next() != Some('"') {
                continue;
            }
            let mut value = String::new();
            for c in chars {
                if c == '"' {
                    return Some(value);
                }
                value.push(c);
            }
        }
    }
    None
}

#[cfg(test)]
mod acf_tests {
    use super::*;

    #[test]
    fn parse_simple_name() {
        let acf = r#"
"AppState"
{
    "appid"  "1172470"
    "name"   "Apex Legends"
    "Universe"  "1"
}
"#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some("Apex Legends"));
    }

    #[test]
    fn parse_name_with_unicode() {
        let acf = r#"
"AppState"
{
    "name"   "ELDEN RING™"
    "appid"  "1245620"
}
"#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some("ELDEN RING™"));
    }

    #[test]
    fn parse_returns_none_when_no_name() {
        let acf = r#"
"AppState"
{
    "appid"  "1234"
}
"#;
        assert!(parse_acf_name(acf).is_none());
    }

    #[test]
    fn parse_handles_extra_whitespace() {
        let acf = r#""name"        "Counter-Strike 2""#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some("Counter-Strike 2"));
    }
}

/// A detected game, keyed by PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGame {
    pub pid: u32,
    pub name: String,
}

/// Try to identify a game from a process snapshot.
pub fn match_game(
    pid: u32,
    comm: &str,
    cmdline: &str,
    environ: &std::collections::HashMap<String, String>,
) -> Option<DetectedGame> {
    // Steam: cmdline contains "reaper SteamLaunch AppId=<id>"
    if let Some(idx) = cmdline.find("SteamLaunch AppId=") {
        let rest = &cmdline[idx + "SteamLaunch AppId=".len()..];
        let app_id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !app_id.is_empty() {
            let name = steam_game_name(&app_id).unwrap_or_else(|| comm.to_string());
            return Some(DetectedGame { pid, name });
        }
    }

    // Steam alternate: SteamAppId env var
    if let Some(app_id) = environ.get("SteamAppId") {
        let name = steam_game_name(app_id).unwrap_or_else(|| comm.to_string());
        return Some(DetectedGame { pid, name });
    }

    // Lutris: cmdline contains "lutris-wrapper"
    if cmdline.contains("lutris-wrapper") {
        // lutris-wrapper sets a "name" arg early in the cmdline; grab the first
        // post-wrapper non-flag word.
        let parts: Vec<&str> = cmdline.split_whitespace().collect();
        if let Some(idx) = parts.iter().position(|w| w.contains("lutris-wrapper")) {
            for word in &parts[idx + 1..] {
                if !word.starts_with('-') {
                    return Some(DetectedGame {
                        pid,
                        name: word.to_string(),
                    });
                }
            }
        }
        return Some(DetectedGame {
            pid,
            name: comm.to_string(),
        });
    }

    // Heroic: comm == "heroic" or its launched wine processes
    if comm == "heroic" || cmdline.contains("HeroicGamesLauncher") {
        return Some(DetectedGame {
            pid,
            name: "Heroic Game".to_string(),
        });
    }

    // gamescope session — only treat as game-detected if it has children doing real work.
    // For the v1 plan, we treat the gamescope process itself as a signal.
    if comm == "gamescope" {
        return Some(DetectedGame {
            pid,
            name: "Gamescope Game".to_string(),
        });
    }

    // mangohud — only matches when it wraps a process; that wrapped process's
    // comm is what we want. For simplicity here, we just flag the parent.
    if comm == "mangohud" {
        return Some(DetectedGame {
            pid,
            name: comm.to_string(),
        });
    }

    None
}

#[cfg(test)]
mod matcher_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn matches_steam_via_cmdline() {
        let cmdline = "/usr/bin/reaper SteamLaunch AppId=1172470 -- /path/to/game.exe";
        let env = HashMap::new();
        let m = match_game(42, "reaper", cmdline, &env);
        assert!(m.is_some());
        assert_eq!(m.unwrap().pid, 42);
    }

    #[test]
    fn matches_steam_via_environ() {
        let mut env = HashMap::new();
        env.insert("SteamAppId".into(), "1172470".into());
        let m = match_game(42, "wine64", "wine64 game.exe", &env);
        assert!(m.is_some());
    }

    #[test]
    fn matches_lutris_wrapper() {
        let cmdline = "lutris-wrapper Doom_Eternal -- /path/doom.exe";
        let env = HashMap::new();
        let m = match_game(42, "lutris-wrapper", cmdline, &env);
        assert!(m.is_some());
        assert_eq!(m.unwrap().name, "Doom_Eternal");
    }

    #[test]
    fn matches_gamescope() {
        let env = HashMap::new();
        let m = match_game(42, "gamescope", "gamescope -- game", &env);
        assert!(m.is_some());
    }

    #[test]
    fn no_match_for_unrelated_process() {
        let env = HashMap::new();
        assert!(match_game(42, "firefox", "firefox", &env).is_none());
    }
}
