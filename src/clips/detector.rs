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
///
/// Note: joining argv with spaces is lossy — `["foo bar", "baz"]` and
/// `["foo", "bar baz"]` produce the same string. Matchers that grep for
/// substrings (`SteamLaunch AppId=`, `lutris-wrapper`) are unaffected since
/// those tokens never span argv boundaries. A future matcher that needs
/// per-arg precision should use the raw NUL-separated bytes directly.
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
            let mut prev = ' ';
            for c in chars {
                if c == '"' && prev != '\\' {
                    return Some(value);
                }
                // Strip backslash before quote (consume escape).
                if c == '"' && prev == '\\' {
                    value.pop(); // remove the backslash we already pushed
                }
                value.push(c);
                prev = c;
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

    #[test]
    fn parse_name_with_escaped_quotes() {
        let acf = r#""name"   "Devil May Cry: \"Special Edition\"""#;
        assert_eq!(parse_acf_name(acf).as_deref(), Some(r#"Devil May Cry: "Special Edition""#));
    }
}

/// A detected game, keyed by PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGame {
    pub pid: u32,
    pub name: String,
}

/// Try to identify a game from a process snapshot.
///
/// Matchers are evaluated top-down and **first match wins**. Order matters because
/// launchers nest — Steam wraps gamescope, gamescope wraps mangohud — so checking
/// Steam-cmdline before gamescope-comm gives us the best name. If you add a new
/// matcher, place it according to specificity: more-specific patterns above
/// less-specific ones.
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

/// Output event from the detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectorEvent {
    GameStarted(DetectedGame),
    GameStopped { pid: u32 },
}

/// Stateful detector — call `tick` every 2 seconds with the current process snapshot;
/// returns events.
pub struct GameDetector {
    /// Maps PID → consecutive scans seen.
    seen: std::collections::HashMap<u32, (DetectedGame, u32)>,
    /// Maps PID → consecutive scans missed.
    pending_remove: std::collections::HashMap<u32, u32>,
    /// PIDs we've already announced as started.
    announced: std::collections::HashSet<u32>,
}

impl GameDetector {
    pub fn new() -> Self {
        Self {
            seen: Default::default(),
            pending_remove: Default::default(),
            announced: Default::default(),
        }
    }

    /// Process a snapshot of currently-detected games. Returns events for any
    /// state transitions (new games announced after 2 consecutive scans; removals
    /// announced after 2 consecutive misses).
    pub fn tick(&mut self, current: &[DetectedGame]) -> Vec<DetectorEvent> {
        let mut events = Vec::new();
        let current_pids: std::collections::HashSet<u32> =
            current.iter().map(|g| g.pid).collect();

        // Bump persistence count for each currently-visible game.
        for game in current {
            let entry = self.seen.entry(game.pid).or_insert((game.clone(), 0));
            entry.1 += 1;
            if entry.1 >= 2 && !self.announced.contains(&game.pid) {
                events.push(DetectorEvent::GameStarted(game.clone()));
                self.announced.insert(game.pid);
            }
            self.pending_remove.remove(&game.pid);
        }

        // Bump miss count for any seen but absent.
        let absent: Vec<u32> = self
            .seen
            .keys()
            .copied()
            .filter(|p| !current_pids.contains(p))
            .collect();
        for pid in &absent {
            let count = self.pending_remove.entry(*pid).or_insert(0);
            *count += 1;
            if *count >= 2 {
                if self.announced.remove(pid) {
                    events.push(DetectorEvent::GameStopped { pid: *pid });
                }
                self.seen.remove(pid);
                self.pending_remove.remove(pid);
            }
        }

        events
    }
}

impl Default for GameDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod detector_tests {
    use super::*;

    fn g(pid: u32) -> DetectedGame {
        DetectedGame {
            pid,
            name: "Test Game".into(),
        }
    }

    #[test]
    fn detects_after_two_consecutive_scans() {
        let mut d = GameDetector::new();
        assert!(d.tick(&[g(42)]).is_empty(), "first scan should not fire");
        let evts = d.tick(&[g(42)]);
        assert_eq!(evts.len(), 1);
        assert!(matches!(evts[0], DetectorEvent::GameStarted(_)));
    }

    #[test]
    fn does_not_double_fire() {
        let mut d = GameDetector::new();
        d.tick(&[g(42)]);
        d.tick(&[g(42)]);
        assert!(
            d.tick(&[g(42)]).is_empty(),
            "no re-fire on continued presence"
        );
    }

    #[test]
    fn removes_after_two_consecutive_misses() {
        let mut d = GameDetector::new();
        d.tick(&[g(42)]);
        d.tick(&[g(42)]); // armed
        assert!(d.tick(&[]).is_empty(), "first miss does not fire");
        let evts = d.tick(&[]);
        assert_eq!(evts.len(), 1);
        assert!(matches!(evts[0], DetectorEvent::GameStopped { pid: 42 }));
    }

    #[test]
    fn brief_disappearance_does_not_fire_remove() {
        let mut d = GameDetector::new();
        d.tick(&[g(42)]);
        d.tick(&[g(42)]); // armed
        d.tick(&[]); // miss 1
        let evts = d.tick(&[g(42)]); // back!
        assert!(
            evts.is_empty(),
            "no event when game returns within debounce window"
        );
    }
}

/// Scan the current process tree once and return identified games.
pub fn scan_once() -> Vec<DetectedGame> {
    let mut games = Vec::new();
    for pid in all_pids() {
        let comm = read_comm(pid).unwrap_or_default();
        let cmdline = read_cmdline(pid).unwrap_or_default();
        let environ = read_environ(pid).unwrap_or_default();
        if let Some(g) = match_game(pid, &comm, &cmdline, &environ) {
            games.push(g);
        }
    }
    games
}
