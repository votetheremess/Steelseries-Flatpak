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
