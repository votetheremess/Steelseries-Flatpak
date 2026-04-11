# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Linux ChatMix implementation for the **SteelSeries Arctis Nova Elite** (USB `1038:2244`). SteelSeries provides no Linux software for this headset; the GameDAC's ChatMix feature is Windows-only via SteelSeries GG. This project reverse-engineered the HID protocol and reimplements ChatMix by creating virtual PipeWire sinks and reading dial events from the device.

The eventual goal is a Rust + GTK4/libadwaita Flatpak app. Current state is Phase 2: working CLI that creates sinks and responds to dial events. Phase 3 (GUI) is not yet started.

## Commands

```bash
# Build
cargo build

# Run (requires Arctis Nova Elite plugged in)
./target/debug/arctis-chatmix

# Tests (12 unit tests for protocol parsing + mixer math)
cargo test

# Run a single test
cargo test parse_dial_position
```

**Bazzite note**: Rust is installed on the host via rustup. GTK4/libadwaita dev packages live in a distrobox and will be needed for Phase 3 but aren't needed for the current CLI.

## Architecture — how the pieces fit

Three layers, each in its own module:

1. **HID layer** (`src/hid/`) — direct `/dev/hidraw*` access, no `hidapi` dependency (avoids pulling `libudev-sys` which needs system headers absent on immutable Fedora).
   - `device.rs` scans `/sys/class/hidraw/*/device/uevent` to find the device by VID/PID/interface number, opens the hidraw char device read+write with `O_NONBLOCK`. Uses raw `libc::poll` + `File::read`/`write_all`.
   - `protocol.rs` parses 64-byte HID reports (`07 [feature] [value] [value2] 00...`) into a `HidEvent` enum. **Writes** the ChatMix enable/disable commands (`06 49 01` / `06 49 00`).
   - `listener.rs` spawns a dedicated `std::thread` that does blocking reads and sends events via `mpsc::Sender`.

2. **Audio layer** (`src/audio/`) — shells out to `pw-cli` and `pactl` (no PipeWire Rust bindings needed).
   - `sinks.rs` creates `ChatMix_Game` and `ChatMix_Chat` null sinks via `pw-cli create-node adapter`. See **critical note below** about required properties.
   - `router.rs` creates loopback modules via `pactl load-module module-loopback` to pipe the virtual sinks into the physical headset output. Auto-detects the headset sink by grepping `pactl list sinks short` for `SteelSeries.*Arctis_Nova_Elite`.
   - `mixer.rs` has `dial_to_volumes()` — currently unused but kept for the Volume-mode fallback path; the primary path uses device-reported ChatMix levels directly.

3. **Orchestration** (`src/main.rs`) — runs sinks → router → device open → write enable command → HID listener thread → event loop that calls `pactl set-sink-volume` on each `ChatMixLevels` event. Cleanup via `Drop` on `VirtualSinks` and `AudioRouter`.

## Protocol — confirmed bytes (reverse-engineered live)

All events on `/dev/hidraw` matching interface 3 (64-byte packets). Format: `07 [feature] [byte2] [byte3] 00...`

| Feature | Meaning | Bytes |
|---|---|---|
| `0x25` | Dial position (**Volume mode** only, 0–15) | byte2 = position |
| `0x45` | **ChatMix levels** (after enable command) | byte2 = game 0–100, byte3 = chat 0–100 |
| `0xBD` | Noise control | `0x00`=Off, `0x01`=Transparency, `0x02`=ANC |
| `0xB8` | ANC hardware event | `0x03` observed |

**ChatMix enable command (write to hidraw)**: `06 49 01 00 00 ... 00` — 64 bytes, zero-padded. This is the critical discovery that makes proper ChatMix work on the Elite. Without it, ChatMix mode on the GameDAC sends nothing; only Volume mode sends `0x25` events.

**Dial click sends nothing** — the mode switch (Volume ↔ ChatMix) is entirely internal to the GameDAC. There's no way to detect which mode it's in from software.

## Critical gotchas

- **`pw-cli` null sinks require `monitor.channel-volumes=true` and `monitor.passthrough=true`** in their props, or `pactl set-sink-volume` will be silently ignored (volume reports as changed, audio is unaffected). `pactl load-module module-null-sink` sets these automatically but truncates descriptions with spaces, so we use `pw-cli` + these properties.
- **Sink names must use underscores**, not spaces (`ChatMix_Game` not `ChatMix Game`). Display names come from `node.description` which DOES support spaces when passed via `pw-cli` — not via `pactl load-module` (which truncates).
- **Don't use `hidapi` crate** — its `linux-native` feature still pulls `libudev-sys` which needs `systemd-devel` (not available on Bazzite without rpm-ostree layering). We use raw `/dev/hidraw` access instead.
- **`timeout` kills without running Drop handlers** — orphaned sinks/loopbacks will accumulate. `VirtualSinks::create()` calls `cleanup_orphaned()` on startup to recover.
- **udev rule at `udev/71-arctis-nova-elite.rules`** grants `MODE="0666"` on hidraw nodes for VID `1038` PID `2244`. Must be installed to `/etc/udev/rules.d/` on the host.

## Why certain dependencies are absent

- No `hidapi` — see above
- No `pipewire-rs` — lacks `create_object` for creating sinks; shelling out to `pw-cli`/`pactl` is what every comparable project does
- No `tokio` — GTK has its own main loop (once we add GUI); `std::thread` + `mpsc` is sufficient
- No `gtk4`/`libadwaita` yet — Phase 3 hasn't started
