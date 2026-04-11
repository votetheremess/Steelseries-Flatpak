use std::fmt;

const REPORT_EVENT: u8 = 0x07;
const REPORT_RESPONSE: u8 = 0x01;
const FEATURE_DIAL: u8 = 0x25;
const FEATURE_CHATMIX: u8 = 0x45;
const FEATURE_NOISE_CONTROL: u8 = 0xBD;
const FEATURE_ANC_HARDWARE: u8 = 0xB8;
const FEATURE_BATTERY: u8 = 0xB7;

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
    BatteryStatus { headset: u8, spare: u8, flags: u8 },
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
            HidEvent::BatteryStatus { headset, spare, flags } => {
                write!(f, "Battery: headset={headset}%, spare={spare}%, flags=0x{flags:02x}")
            }
            HidEvent::Unknown { feature, value } => {
                write!(f, "Unknown event: feature=0x{feature:02x} value=0x{value:02x}")
            }
        }
    }
}

pub fn parse(buf: &[u8; 64]) -> Option<HidEvent> {
    let report_id = buf[0];
    let feature = buf[1];

    match report_id {
        REPORT_EVENT => parse_event(feature, buf),
        REPORT_RESPONSE => parse_response(feature, buf),
        _ => {
            log::debug!("Unexpected report ID: 0x{report_id:02x}");
            None
        }
    }
}

fn parse_event(feature: u8, buf: &[u8; 64]) -> Option<HidEvent> {
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
            log::debug!("Unknown event feature: 0x{feature:02x} value: 0x{value:02x}");
            HidEvent::Unknown { feature, value }
        }
    })
}

fn parse_response(feature: u8, buf: &[u8; 64]) -> Option<HidEvent> {
    Some(match feature {
        FEATURE_BATTERY => HidEvent::BatteryStatus {
            headset: buf[2],
            spare: buf[3],
            flags: buf[4],
        },
        _ => {
            log::debug!(
                "Unknown response feature: 0x{feature:02x} bytes: {:02x?}",
                &buf[2..8]
            );
            HidEvent::Unknown {
                feature,
                value: buf[2],
            }
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

pub fn battery_query_command() -> [u8; 64] {
    let mut buf = [0u8; 64];
    buf[0] = 0x06;
    buf[1] = 0xB7;
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(feature: u8, value: u8) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0] = 0x07;
        buf[1] = feature;
        buf[2] = value;
        buf
    }

    fn make_response(feature: u8, b2: u8, b3: u8, b4: u8) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0] = 0x01;
        buf[1] = feature;
        buf[2] = b2;
        buf[3] = b3;
        buf[4] = b4;
        buf
    }

    #[test]
    fn parse_dial_position() {
        let event = parse(&make_event(0x25, 0x07)).unwrap();
        assert_eq!(event, HidEvent::DialPosition(7));
    }

    #[test]
    fn parse_chatmix_levels() {
        let mut buf = make_event(0x45, 50);
        buf[3] = 75;
        let event = parse(&buf).unwrap();
        assert_eq!(event, HidEvent::ChatMixLevels { game: 50, chat: 75 });
    }

    #[test]
    fn parse_noise_control_anc() {
        let event = parse(&make_event(0xBD, 0x02)).unwrap();
        assert_eq!(event, HidEvent::NoiseControl(NoiseMode::Anc));
    }

    #[test]
    fn parse_noise_control_transparency() {
        let event = parse(&make_event(0xBD, 0x01)).unwrap();
        assert_eq!(event, HidEvent::NoiseControl(NoiseMode::Transparency));
    }

    #[test]
    fn parse_noise_control_off() {
        let event = parse(&make_event(0xBD, 0x00)).unwrap();
        assert_eq!(event, HidEvent::NoiseControl(NoiseMode::Off));
    }

    #[test]
    fn parse_anc_hardware() {
        let event = parse(&make_event(0xB8, 0x03)).unwrap();
        assert_eq!(event, HidEvent::AncHardware(0x03));
    }

    #[test]
    fn parse_battery_response() {
        let event = parse(&make_response(0xB7, 0x60, 0x64, 0x08)).unwrap();
        assert_eq!(
            event,
            HidEvent::BatteryStatus {
                headset: 0x60,
                spare: 0x64,
                flags: 0x08,
            }
        );
    }

    #[test]
    fn parse_unknown_feature() {
        let event = parse(&make_event(0xFA, 0x42)).unwrap();
        assert_eq!(event, HidEvent::Unknown { feature: 0xFA, value: 0x42 });
    }

    #[test]
    fn parse_wrong_report_id() {
        let mut buf = [0u8; 64];
        buf[0] = 0x99;
        buf[1] = 0x25;
        buf[2] = 0x05;
        assert_eq!(parse(&buf), None);
    }
}
