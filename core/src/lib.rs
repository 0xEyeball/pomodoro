//! Toolkit-independent core of the Pomodoro app: timer engine, storage, statistics and backups.

pub mod backup;
pub mod db;
pub mod engine;
pub mod model;
pub mod settings;
pub mod stats;
pub mod time;

pub const APP_ID: &str = "io.github._0xEyeball.Pomodoro";
