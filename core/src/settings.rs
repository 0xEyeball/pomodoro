//! User preferences, stored as TOML in `~/.config/<app-id>/settings.toml`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{Mode, PhaseKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub focus_min: u32,
    pub short_break_min: u32,
    pub long_break_min: u32,
    pub pomodoros_per_session: u32,
    pub auto_start: bool,

    pub default_mode: Mode,
    pub remember_last_tag: bool,
    pub last_tag_id: Option<String>,

    pub goal_sessions: u32,
    pub day_start_hour: u32,

    pub sound_enabled: bool,
    pub volume: f64,
    pub separate_break_sound: bool,
    pub notifications: bool,

    pub appearance: Appearance,
    pub auto_backup: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            focus_min: 25,
            short_break_min: 5,
            long_break_min: 15,
            pomodoros_per_session: 4,
            auto_start: false,
            default_mode: Mode::Standard,
            remember_last_tag: true,
            last_tag_id: None,
            goal_sessions: 2,
            day_start_hour: 0,
            sound_enabled: true,
            volume: 0.5,
            separate_break_sound: false,
            notifications: true,
            appearance: Appearance::System,
            auto_backup: false,
        }
    }
}

pub const FOCUS_RANGE: (u32, u32) = (1, 120);
pub const SHORT_BREAK_RANGE: (u32, u32) = (1, 60);
pub const LONG_BREAK_RANGE: (u32, u32) = (1, 90);
pub const POMODOROS_RANGE: (u32, u32) = (1, 12);
pub const GOAL_RANGE: (u32, u32) = (1, 10);

impl Settings {
    /// Forces every value into its allowed range.
    pub fn clamped(mut self) -> Self {
        let c = |v: u32, (lo, hi): (u32, u32)| v.clamp(lo, hi);
        self.focus_min = c(self.focus_min, FOCUS_RANGE);
        self.short_break_min = c(self.short_break_min, SHORT_BREAK_RANGE);
        self.long_break_min = c(self.long_break_min, LONG_BREAK_RANGE);
        self.pomodoros_per_session = c(self.pomodoros_per_session, POMODOROS_RANGE);
        self.goal_sessions = c(self.goal_sessions, GOAL_RANGE);
        self.day_start_hour = self.day_start_hour.min(23);
        self.volume = if self.volume.is_finite() { self.volume.clamp(0.0, 1.0) } else { 0.5 };
        self
    }

    pub fn phase_sec(&self, kind: PhaseKind) -> u32 {
        60 * match kind {
            PhaseKind::Focus => self.focus_min,
            PhaseKind::ShortBreak => self.short_break_min,
            PhaseKind::LongBreak => self.long_break_min,
        }
    }

    /// Loads settings; a missing file yields defaults, a malformed one is an error.
    pub fn load(path: &Path) -> Result<Settings, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Settings>(&text)
                .map(Settings::clamped)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!((s.focus_min, s.short_break_min, s.long_break_min), (25, 5, 15));
        assert_eq!(s.pomodoros_per_session, 4);
        assert_eq!(s.goal_sessions, 2);
        assert_eq!(s.default_mode, Mode::Standard);
        assert!(!s.auto_start);
    }

    #[test]
    fn roundtrip_and_partial_files() {
        let dir = std::env::temp_dir().join(format!("pomo-settings-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.toml");
        let s = Settings { focus_min: 50, ..Settings::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);

        std::fs::write(&path, "focus_min = 500\n").unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.focus_min, 120);
        assert_eq!(loaded.short_break_min, 5);
        std::fs::remove_dir_all(dir).ok();
    }
}
