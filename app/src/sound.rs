//! Chime playback through GTK's media stream (GStreamer backend).

use std::cell::RefCell;
use std::path::PathBuf;

use gtk::glib;
use gtk::prelude::*;
use pomodoro_core::settings::Settings;
use pomodoro_core::APP_ID;

const CHIME: &[u8] = include_bytes!("../../data/sounds/chime.wav");
const CHIME_LOW: &[u8] = include_bytes!("../../data/sounds/chime-low.wav");

/// Where `make install` / the PKGBUILD puts the sounds.
const SYSTEM_SOUND_DIR: Option<&str> = option_env!("POMODORO_SOUND_DIR");

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Chime {
    FocusEnd,
    BreakEnd,
}

pub struct Sound {
    playing: RefCell<Option<gtk::MediaFile>>,
}

impl Sound {
    pub fn new() -> Sound {
        Sound { playing: RefCell::new(None) }
    }

    pub fn play(&self, chime: Chime, settings: &Settings) {
        if !settings.sound_enabled {
            return;
        }
        let low = chime == Chime::BreakEnd && settings.separate_break_sound;
        self.play_at(low, settings.volume);
    }

    pub fn play_at(&self, low: bool, volume: f64) {
        let Some(path) = sound_path(low) else { return };
        let media = gtk::MediaFile::for_filename(&path);
        media.set_volume(volume.clamp(0.0, 1.0));
        media.play();
        // Keep it alive until the next chime replaces it.
        *self.playing.borrow_mut() = Some(media);
    }
}

/// The installed sound file, or the embedded copy written to the cache directory.
fn sound_path(low: bool) -> Option<PathBuf> {
    let name = if low { "chime-low.wav" } else { "chime.wav" };
    if let Some(dir) = SYSTEM_SOUND_DIR {
        let p = PathBuf::from(dir).join(name);
        if p.exists() {
            return Some(p);
        }
    }
    let bytes = if low { CHIME_LOW } else { CHIME };
    let p = glib::user_cache_dir().join(APP_ID).join(name);
    let fresh = std::fs::metadata(&p).map(|m| m.len() == bytes.len() as u64).unwrap_or(false);
    if !fresh {
        std::fs::create_dir_all(p.parent()?).ok()?;
        std::fs::write(&p, bytes).ok()?;
    }
    Some(p)
}
