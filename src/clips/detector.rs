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
