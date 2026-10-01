//! The equalizer: ten ISO octave bands from 31 Hz to 16 kHz, ±12 dB, with
//! presets. In mpv it is one labelled lavfi graph in `af` (a headroom
//! `volume` and ten peaking `equalizer`s), so a band changes live with
//! `af-command` instead of rebuilding the chain; Flat or off removes it.

use serde::{Deserialize, Serialize};

/// Centre frequencies in Hz.
pub const BANDS: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
/// The largest cut or boost of a band, in dB.
pub const RANGE: f32 = 12.0;
/// The filter's label in mpv's `af`.
pub const LABEL: &str = "ytfast-eq";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    Flat,
    BassBoost,
    TrebleBoost,
    Vocal,
    Rock,
    Electronic,
    Acoustic,
    LateNight,
    /// Bands edited by hand.
    Custom,
}

impl Preset {
    /// The presets offered, in the order shown.
    pub const ALL: [Preset; 8] = [
        Preset::Flat,
        Preset::BassBoost,
        Preset::TrebleBoost,
        Preset::Vocal,
        Preset::Rock,
        Preset::Electronic,
        Preset::Acoustic,
        Preset::LateNight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Flat => "Flat",
            Preset::BassBoost => "Bass boost",
            Preset::TrebleBoost => "Treble boost",
            Preset::Vocal => "Vocal",
            Preset::Rock => "Rock",
            Preset::Electronic => "Electronic",
            Preset::Acoustic => "Acoustic",
            Preset::LateNight => "Late night",
            Preset::Custom => "Custom",
        }
    }

    /// The band gains in dB; `None` for Custom.
    pub fn gains(self) -> Option<[f32; 10]> {
        Some(match self {
            Preset::Flat => [0.0; 10],
            Preset::BassBoost => [6.0, 5.0, 4.0, 2.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            Preset::TrebleBoost => [0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 2.0, 4.0, 5.0, 6.0],
            Preset::Vocal => [-2.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0, 3.0, 1.0, 0.0],
            Preset::Rock => [4.5, 3.5, 2.0, 0.0, -1.0, -1.0, 1.0, 2.5, 3.5, 4.0],
            Preset::Electronic => [5.0, 4.0, 1.5, 0.0, -2.0, 1.0, 0.0, 1.5, 4.0, 5.0],
            Preset::Acoustic => [3.0, 3.0, 2.0, 1.0, 1.5, 1.5, 2.5, 3.0, 2.5, 1.5],
            // Quiet listening: less low end to carry through walls, a little
            // more presence so voices stay clear at low volume.
            Preset::LateNight => [-6.0, -4.5, -2.5, -0.5, 0.5, 1.5, 2.0, 1.0, -0.5, -2.0],
            Preset::Custom => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Equalizer {
    /// Off bypasses the filter and keeps the bands.
    pub enabled: bool,
    pub preset: Preset,
    /// Gain per band of [`BANDS`], in dB.
    pub gains: [f32; 10],
}

impl Default for Equalizer {
    fn default() -> Self {
        Self {
            enabled: true,
            preset: Preset::Flat,
            gains: [0.0; 10],
        }
    }
}

impl Equalizer {
    pub fn with_preset(&self, preset: Preset) -> Self {
        Self {
            enabled: true,
            preset,
            gains: preset.gains().unwrap_or(self.gains),
        }
    }

    /// One band set by hand: a preset it matches, or Custom.
    pub fn with_band(&self, band: usize, gain: f32) -> Self {
        let mut gains = self.gains;
        if let Some(g) = gains.get_mut(band) {
            *g = (gain * 2.0).round() / 2.0;
            *g = g.clamp(-RANGE, RANGE);
        }
        let preset = Preset::ALL
            .into_iter()
            .find(|p| p.gains() == Some(gains))
            .unwrap_or(Preset::Custom);
        Self {
            enabled: true,
            preset,
            gains,
        }
    }

    /// Whether it changes the sound at all.
    pub fn active(&self) -> bool {
        self.enabled && self.gains.iter().any(|g| g.abs() >= 0.05)
    }

    /// Headroom in dB (≤ 0), so boosted bands don't clip.
    pub fn preamp(&self) -> f32 {
        -self.gains.iter().fold(0.0_f32, |m, g| m.max(*g))
    }

    /// mpv's `af` value: empty when bypassed or flat.
    pub fn filter(&self) -> String {
        if !self.active() {
            return String::new();
        }
        let bands: Vec<String> = BANDS
            .iter()
            .zip(self.gains)
            .enumerate()
            .map(|(i, (f, g))| format!("equalizer@b{i}=f={f}:t=o:w=1:g={g:.1}"))
            .collect();
        format!(
            "@{LABEL}:lavfi=[volume@pre=volume={:.1}dB,{}]",
            self.preamp(),
            bands.join(",")
        )
    }

    /// "31 Hz", "1 kHz".
    pub fn band_label(band: usize) -> String {
        match BANDS.get(band) {
            Some(&f) if f >= 1000 => format!("{} kHz", f / 1000),
            Some(f) => format!("{f} Hz"),
            None => String::new(),
        }
    }
}
