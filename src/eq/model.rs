use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterType {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

impl FilterType {
    pub const ALL: &[FilterType] = &[
        FilterType::Peaking,
        FilterType::LowShelf,
        FilterType::HighShelf,
        FilterType::LowPass,
        FilterType::HighPass,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterType::Peaking => "Peaking",
            FilterType::LowShelf => "Low Shelf",
            FilterType::HighShelf => "High Shelf",
            FilterType::LowPass => "Low Pass",
            FilterType::HighPass => "High Pass",
        }
    }

    pub fn from_str(s: &str) -> Option<FilterType> {
        match s {
            "peaking" => Some(FilterType::Peaking),
            "low_shelf" => Some(FilterType::LowShelf),
            "high_shelf" => Some(FilterType::HighShelf),
            "low_pass" => Some(FilterType::LowPass),
            "high_pass" => Some(FilterType::HighPass),
            _ => None,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            FilterType::Peaking => "peaking",
            FilterType::LowShelf => "low_shelf",
            FilterType::HighShelf => "high_shelf",
            FilterType::LowPass => "low_pass",
            FilterType::HighPass => "high_pass",
        }
    }

    /// Whether this filter type uses the gain parameter.
    /// LowPass and HighPass are gain-independent.
    pub fn uses_gain(self) -> bool {
        matches!(self, FilterType::Peaking | FilterType::LowShelf | FilterType::HighShelf)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Band {
    pub frequency: f64,
    pub gain_db: f64,
    pub q: f64,
    pub filter_type: FilterType,
    pub enabled: bool,
}

impl Band {
    pub fn new(frequency: f64) -> Self {
        Band {
            frequency,
            gain_db: 0.0,
            q: 1.0,
            filter_type: FilterType::Peaking,
            enabled: true,
        }
    }
}

pub const NUM_BANDS: usize = 10;
pub const FREQ_MIN: f64 = 20.0;
pub const FREQ_MAX: f64 = 20000.0;
pub const GAIN_MIN: f64 = -12.0;
pub const GAIN_MAX: f64 = 12.0;
pub const Q_MIN: f64 = 0.1;
pub const Q_MAX: f64 = 10.0;

pub const DEFAULT_FREQUENCIES: [f64; NUM_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// RGB colors for the 10 bands (values 0.0–1.0).
pub const BAND_COLORS: [(f64, f64, f64); NUM_BANDS] = [
    (0.90, 0.30, 0.30), // red
    (0.95, 0.55, 0.25), // orange
    (0.95, 0.85, 0.30), // yellow
    (0.50, 0.85, 0.35), // green
    (0.30, 0.80, 0.70), // teal
    (0.30, 0.60, 0.90), // blue
    (0.45, 0.40, 0.90), // indigo
    (0.70, 0.40, 0.90), // violet
    (0.90, 0.40, 0.70), // pink
    (0.60, 0.35, 0.15), // brown
];

pub fn default_bands() -> [Band; NUM_BANDS] {
    std::array::from_fn(|i| Band::new(DEFAULT_FREQUENCIES[i]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EqTarget {
    Game,
    Chat,
    Music,
    Aux,
    Mic,
}

impl EqTarget {
    pub const ALL: &[EqTarget] = &[
        EqTarget::Game,
        EqTarget::Chat,
        EqTarget::Music,
        EqTarget::Aux,
        EqTarget::Mic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqTarget::Game => "Game",
            EqTarget::Chat => "Chat",
            EqTarget::Music => "Music",
            EqTarget::Aux => "Aux",
            EqTarget::Mic => "Mic",
        }
    }

    pub fn sink_name(self) -> &'static str {
        match self {
            EqTarget::Game => "SteelSeries_Game",
            EqTarget::Chat => "SteelSeries_Chat",
            EqTarget::Music => "SteelSeries_Music",
            EqTarget::Aux => "SteelSeries_Aux",
            EqTarget::Mic => "SteelSeries_Mic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SinkEq {
    pub bands: [Band; NUM_BANDS],
    pub preset_name: Option<String>,
}

impl SinkEq {
    pub fn flat() -> Self {
        SinkEq {
            bands: default_bands(),
            preset_name: Some("Flat".to_string()),
        }
    }
}

pub struct EqState {
    pub sinks: HashMap<EqTarget, SinkEq>,
    pub active_target: EqTarget,
    pub selected_band: Option<usize>,
}

impl EqState {
    pub fn new() -> Self {
        let mut sinks = HashMap::new();
        for &target in EqTarget::ALL {
            sinks.insert(target, SinkEq::flat());
        }
        EqState {
            sinks,
            active_target: EqTarget::Game,
            selected_band: None,
        }
    }

    pub fn active_sink_eq(&self) -> &SinkEq {
        &self.sinks[&self.active_target]
    }

    pub fn active_sink_eq_mut(&mut self) -> &mut SinkEq {
        self.sinks.get_mut(&self.active_target).unwrap()
    }

    pub fn selected_band_mut(&mut self) -> Option<&mut Band> {
        let idx = self.selected_band?;
        Some(&mut self.active_sink_eq_mut().bands[idx])
    }
}

pub type SharedEqState = Rc<RefCell<EqState>>;

pub fn new_shared_state() -> SharedEqState {
    Rc::new(RefCell::new(EqState::new()))
}
