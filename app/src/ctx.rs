//! Shared application state handed to every view.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use pomodoro_core::engine::Engine;
use pomodoro_core::model::{Mode, Tag, TagColor};
use pomodoro_core::settings::{Appearance, Settings};
use pomodoro_core::APP_ID;

use crate::sound::Sound;

pub struct Paths {
    pub settings: PathBuf,
    pub data_dir: PathBuf,
    pub db: PathBuf,
    pub backups: PathBuf,
}

impl Paths {
    pub fn new() -> Paths {
        let config_dir = glib::user_config_dir().join(APP_ID);
        let data_dir = glib::user_data_dir().join(APP_ID);
        Paths {
            settings: config_dir.join("settings.toml"),
            db: data_dir.join("pomodoro.db"),
            backups: data_dir.join("backups"),
            data_dir,
        }
    }
}

/// What changed, so listeners can skip work that does not concern them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Timer state (phase, run state, session).
    Timer,
    /// Recorded history (new records, import).
    Data,
    Tags,
    Settings,
}

pub struct Ctx {
    pub app: adw::Application,
    pub paths: Paths,
    pub engine: RefCell<Engine>,
    pub sound: Sound,
    pub window: OnceCell<adw::ApplicationWindow>,
    pub toasts: OnceCell<adw::ToastOverlay>,
    /// Tag and mode chosen for the next session (before one starts).
    pub next_tag: RefCell<Option<String>>,
    pub next_mode: RefCell<Mode>,
    listeners: RefCell<Vec<Listener>>,
}

type Listener = Box<dyn Fn(Change)>;

impl Ctx {
    pub fn new(app: &adw::Application, paths: Paths, engine: Engine) -> Rc<Ctx> {
        let settings = engine.settings().clone();
        let tag = if settings.remember_last_tag { settings.last_tag_id.clone() } else { None };
        // Ignore a remembered tag that was archived or deleted in the meantime.
        let tag = tag.filter(|id| engine.db().tag(id).ok().flatten().is_some_and(|t| !t.archived));
        let ctx = Rc::new(Ctx {
            app: app.clone(),
            paths,
            engine: RefCell::new(engine),
            sound: Sound::new(),
            window: OnceCell::new(),
            toasts: OnceCell::new(),
            next_tag: RefCell::new(tag),
            next_mode: RefCell::new(settings.default_mode),
            listeners: RefCell::new(Vec::new()),
        });
        ctx.apply_settings_side_effects(&settings);
        ctx
    }

    pub fn settings(&self) -> Settings {
        self.engine.borrow().settings().clone()
    }

    /// Edits, persists and applies settings, then notifies listeners.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.settings();
        let before = s.clone();
        f(&mut s);
        let s = s.clamped();
        if s == before {
            return;
        }
        if let Err(e) = s.save(&self.paths.settings) {
            self.toast(&format!("Could not save settings: {e}"));
        }
        let r = self.engine.borrow_mut().set_settings(s.clone());
        self.report(r);
        self.apply_settings_side_effects(&s);
        self.notify(Change::Settings);
    }

    fn apply_settings_side_effects(&self, s: &Settings) {
        adw::StyleManager::default().set_color_scheme(match s.appearance {
            Appearance::System => adw::ColorScheme::Default,
            Appearance::Light => adw::ColorScheme::ForceLight,
            Appearance::Dark => adw::ColorScheme::ForceDark,
        });
    }

    pub fn subscribe(&self, f: impl Fn(Change) + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    pub fn notify(&self, change: Change) {
        for l in self.listeners.borrow().iter() {
            l(change);
        }
    }

    pub fn toast(&self, msg: &str) {
        if let Some(t) = self.toasts.get() {
            t.add_toast(adw::Toast::new(msg));
        } else {
            eprintln!("{msg}");
        }
    }

    /// Shows an error toast for a failed result and returns the success value.
    pub fn report<T, E: std::fmt::Display>(&self, r: Result<T, E>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.toast(&e.to_string());
                None
            }
        }
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        self.window.get().expect("window initialised")
    }

    /// Active (non-archived) tags, sorted by name.
    pub fn active_tags(&self) -> Vec<Tag> {
        let tags = self.engine.borrow().db().tags();
        self.report(tags).unwrap_or_default().into_iter().filter(|t| !t.archived).collect()
    }

    pub fn all_tags(&self) -> Vec<Tag> {
        let tags = self.engine.borrow().db().tags();
        self.report(tags).unwrap_or_default()
    }
}

/// CSS class carrying a tag colour, e.g. `tag-blue`.
pub fn tag_class(color: TagColor) -> String {
    format!("tag-{}", color.as_str())
}

/// The libadwaita palette colour used for each tag colour.
fn palette(color: TagColor) -> &'static str {
    match color {
        TagColor::Blue => "@blue_3",
        TagColor::Green => "@green_4",
        TagColor::Yellow => "@yellow_5",
        TagColor::Orange => "@orange_3",
        TagColor::Red => "@red_3",
        TagColor::Purple => "@purple_3",
        TagColor::Brown => "@brown_2",
        TagColor::Slate => "@dark_1",
    }
}

const HEAT_ALPHA: [f64; 4] = [0.3, 0.55, 0.8, 1.0];

pub fn load_css() {
    let mut css = String::from(include_str!("style.css"));
    // Heatmap intensities for the accent colour and every tag colour. Levels differ in
    // opacity (lightness), not only in hue.
    let mut add_levels = |scope: &str, color: &str| {
        for (i, a) in HEAT_ALPHA.iter().enumerate() {
            css.push_str(&format!(
                "{scope} .heat-cell.heat-{} {{ background-color: alpha({color}, {a}); }}\n",
                i + 1
            ));
        }
    };
    add_levels(".heatmap", "@accent_bg_color");
    for &c in TagColor::ALL {
        add_levels(&format!(".heatmap.{}", tag_class(c)), palette(c));
    }
    for &c in TagColor::ALL {
        css.push_str(&format!(".tag-dot.{} {{ background-color: {}; }}\n", tag_class(c), palette(c)));
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("a display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// A small coloured circle for a tag (or a hollow one for "no tag").
pub fn tag_dot(color: Option<TagColor>) -> gtk::Box {
    let dot = gtk::Box::builder().valign(gtk::Align::Center).css_classes(["tag-dot"]).build();
    match color {
        Some(c) => dot.add_css_class(&tag_class(c)),
        None => dot.add_css_class("no-tag"),
    }
    dot
}

pub fn set_tag_dot_color(dot: &gtk::Box, color: Option<TagColor>) {
    for &c in TagColor::ALL {
        dot.remove_css_class(&tag_class(c));
    }
    dot.remove_css_class("no-tag");
    match color {
        Some(c) => dot.add_css_class(&tag_class(c)),
        None => dot.add_css_class("no-tag"),
    }
}

/// Gives an icon-only widget a name for screen readers and a tooltip.
pub fn label_widget(w: &impl IsA<gtk::Widget>, label: &str) {
    w.set_tooltip_text(Some(label));
    w.upcast_ref::<gtk::Widget>().update_property(&[gtk::accessible::Property::Label(label)]);
}
