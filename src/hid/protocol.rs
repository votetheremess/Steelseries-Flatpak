use std::fmt;

const REPORT_ID: u8 = 0x07;
const FEATURE_DIAL: u8 = 0x25;
const FEATURE_CHATMIX: u8 = 0x45;
const FEATURE_NOISE_CONTROL: u8 = 0xBD;
const FEATURE_ANC_HARDWARE: u8 = 0xB8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseMode {
    Off,
    Transparency,
    Anc,
}

impl fmt::Display for NoiseMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoiseMode::Off => write!(f, "Off"),
            NoiseMode::Transparency => write!(f, "Transparency"),
            NoiseMode::Anc => write!(f, "Active Noise Canceling"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidEvent {
    DialPosition(u8),
    ChatMixLevels { game: u8, chat: u8 },
    NoiseControl(NoiseMode),
    AncHardware(u8),
    Unknown { feature: u8, value: u8 },
}

impl fmt::Display for HidEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HidEvent::DialPosition(pos) => write!(f, "Dial position: {pos}"),
            HidEvent::ChatMixLevels { game, chat } => {
                write!(f, "ChatMix: game={game}, chat={chat}")
            }
            HidEvent::NoiseControl(mode) => write!(f, "Noise control: {mode}"),
            HidEvent::AncHardware(val) => write!(f, "ANC hardware event: 0x{val:02x}"),
            HidEvent::Unknown { feature, value } => {
                write!(f, "Unknown event: feature=0x{feature:02x} value=0x{value:02x}")
            }
        }
    }
}

pub fn parse(buf: &[u8; 64]) -> Option<HidEvent> {
    if buf[0] != REPORT_ID {
        log::debug!("Unexpected report ID: 0x{:02x}", buf[0]);
        return None;
    }

    let feature = buf[1];
    let value = buf[2];

    Some(match feature {
        FEATURE_DIAL => HidEvent::DialPosition(value),
        FEATURE_CHATMIX => HidEvent::ChatMixLevels {
            game: value,
            chat: buf[3],
        },
        FEATURE_NOISE_CONTROL => {
            let mode = match value {
                0x00 => NoiseMode::Off,
                0x01 => NoiseMode::Transparency,
                0x02 => NoiseMode::Anc,
                other => {
                    log::warn!("Unknown noise control value: 0x{other:02x}");
                    return Some(HidEvent::Unknown { feature, value });
                }
            };
            HidEvent::NoiseControl(mode)
        }
        FEATURE_ANC_HARDWARE => HidEvent::AncHardware(value),
        _ => {
            log::debug!("Unknown feature: 0x{feature:02x} value: 0x{value:02x}");
            HidEvent::Unknown { feature, value }
        }
    })
}

pub fn chatmix_enable_command() -> [u8; 64] {
    let mut buf = [0u8; 64];
    buf[0] = 0x06;
    buf[1] = 0x49;
    buf[2] = 0x01;
    buf
}

pub fn chatmix_disable_command() -> [u8; 64] {
    let mut buf = [0u8; 64];
    buf[0] = 0x06;
    buf[1] = 0x49;
    buf[2] = 0x00;
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_packet(feature: u8, value: u8) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0] = 0x07;
        buf[1] = feature;
        buf[2] = value;
        buf
    }

    #[test]
    fn parse_dial_position() {
        let event = parse(&make_packet(0x25, 0x07)).unwrap();
        assert_eq!(event, HidEvent::DialPosition(7));
    }

    #[test]
    fn parse_noise_control_anc() {
        let event = parse(&make_packet(0xBD, 0x02)).unwrap();
        assert_eq!(event, HidEvent::NoiseControl(NoiseMode::Anc));
    }

    #[test]
    fn parse_noise_control_transparency() {
        let event = parse(&make_packet(0xBD, 0x01)).unwrap();
        assert_eq!(event, HidEvent::NoiseControl(NoiseMode::Transparency));
    }

    #[test]
    fn parse_noise_control_off() {
        let event = parse(&make_packet(0xBD, 0x00)).unwrap();
        assert_eq!(event, HidEvent::NoiseControl(NoiseMode::Off));
    }

    #[test]
    fn parse_anc_hardware() {
        let event = parse(&make_packet(0xB8, 0x03)).unwrap();
        assert_eq!(event, HidEvent::AncHardware(0x03));
    }

    #[test]
    fn parse_unknown_feature() {
        let event = parse(&make_packet(0xFF, 0x42)).unwrap();
        assert_eq!(event, HidEvent::Unknown { feature: 0xFF, value: 0x42 });
    }

    #[test]
    fn parse_wrong_report_id() {
        let mut buf = [0u8; 64];
        buf[0] = 0x01;
        buf[1] = 0x25;
        buf[2] = 0x05;
        assert_eq!(parse(&buf), None);
    }
}
