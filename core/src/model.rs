//! Plain data types shared by storage, the timer engine, statistics and backups.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Declares a string-backed enum with `as_str` / `parse` helpers and lowercase serde names.
macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$variant),)+ _ => None }
            }
        }
    };
}

str_enum!(Mode { Standard => "standard", Blitz => "blitz" });
str_enum!(SessionStatus { Active => "active", Completed => "completed", Cancelled => "cancelled" });
str_enum!(PomodoroStatus { Completed => "completed", Cancelled => "cancelled" });
str_enum!(PhaseKind { Focus => "focus", ShortBreak => "short_break", LongBreak => "long_break" });
str_enum!(RunState { Running => "running", Paused => "paused", Waiting => "waiting" });
str_enum!(
    /// The fixed tag palette. Each maps to a libadwaita palette colour in the UI.
    TagColor {
        Blue => "blue", Green => "green", Yellow => "yellow", Orange => "orange",
        Red => "red", Purple => "purple", Brown => "brown", Slate => "slate",
    }
);

impl PhaseKind {
    pub fn is_break(self) -> bool {
        !matches!(self, PhaseKind::Focus)
    }

    pub fn label(self) -> &'static str {
        match self {
            PhaseKind::Focus => "Focus",
            PhaseKind::ShortBreak => "Short break",
            PhaseKind::LongBreak => "Long break",
        }
    }
}

impl TagColor {
    pub fn label(self) -> &'static str {
        match self {
            TagColor::Blue => "Blue",
            TagColor::Green => "Green",
            TagColor::Yellow => "Yellow",
            TagColor::Orange => "Orange",
            TagColor::Red => "Red",
            TagColor::Purple => "Purple",
            TagColor::Brown => "Brown",
            TagColor::Slate => "Slate",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: TagColor,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub tag_id: Option<String>,
    pub mode: Mode,
    pub target_pomodoros: u32,
    /// RFC 3339 wall-clock timestamp.
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: SessionStatus,
    /// Day the session ended on (after applying the day-start hour).
    pub local_date: Option<NaiveDate>,
    /// Daily goal (in sessions) in force on `local_date`.
    pub goal_sessions: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pomodoro {
    pub id: String,
    pub session_id: String,
    pub started_at: String,
    pub ended_at: String,
    pub planned_sec: u32,
    pub elapsed_sec: f64,
    pub status: PomodoroStatus,
    pub local_date: NaiveDate,
}

impl Pomodoro {
    /// Completed pomodoros count as 1.0; cancelled ones as elapsed / planned (capped at 1).
    pub fn fraction(&self) -> f64 {
        match self.status {
            PomodoroStatus::Completed => 1.0,
            PomodoroStatus::Cancelled => fraction(self.elapsed_sec, self.planned_sec),
        }
    }
}

pub fn fraction(elapsed_sec: f64, planned_sec: u32) -> f64 {
    if planned_sec == 0 {
        return 0.0;
    }
    (elapsed_sec / planned_sec as f64).clamp(0.0, 1.0)
}

/// A daily goal that applies from `effective_from` until the next entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalEntry {
    pub effective_from: NaiveDate,
    pub goal_sessions: u32,
    pub pomodoros_per_session: u32,
}

impl GoalEntry {
    pub fn goal_pomodoros(&self) -> u32 {
        self.goal_sessions * self.pomodoros_per_session
    }
}

/// The single persisted row describing what the timer is doing right now.
///
/// * Idle: no session, no phase, `Waiting`.
/// * Between phases: `phase` is the *next* phase, `Waiting`, elapsed 0.
/// * In a phase: `Running` or `Paused` with the elapsed time accumulated so far.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveState {
    pub session_id: Option<String>,
    pub pomodoro_id: Option<String>,
    pub pomodoro_started_at: Option<String>,
    pub phase: Option<PhaseKind>,
    pub run_state: RunState,
    pub phase_planned_sec: u32,
    pub phase_elapsed_sec: f64,
    pub checkpointed_at: Option<String>,
    pub clean_exit: bool,
}

impl Default for ActiveState {
    fn default() -> Self {
        ActiveState {
            session_id: None,
            pomodoro_id: None,
            pomodoro_started_at: None,
            phase: None,
            run_state: RunState::Waiting,
            phase_planned_sec: 0,
            phase_elapsed_sec: 0.0,
            checkpointed_at: None,
            clean_exit: true,
        }
    }
}
