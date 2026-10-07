//! Main window: header, view switcher, actions, shortcuts, the tick loop and notifications.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use pomodoro_core::backup;
use pomodoro_core::db::Db;
use pomodoro_core::engine::{Completion, Engine};
use pomodoro_core::model::{Mode, PhaseKind};
use pomodoro_core::settings::Settings;
use pomodoro_core::time::SystemClock;

use crate::ctx::{Change, Ctx, Paths};
use crate::sound::Chime;
use crate::stats_view::StatsView;
use crate::timer_view::{window_title, TimerView};
use crate::{dialogs, prefs, suspend};

const CRASH_NOTICE: &str =
    "The app closed unexpectedly. Your pomodoro was restored; up to 5 seconds of progress may be lost.";

pub fn open(app: &adw::Application) {
    let paths = Paths::new();
    let (settings, settings_error) = match Settings::load(&paths.settings) {
        Ok(s) => (s, None),
        Err(e) => (Settings::default(), Some(format!("Settings could not be read, using defaults ({e})"))),
    };
    match Db::open(&paths.db).and_then(|db| Engine::new(db, Box::new(SystemClock::new()), settings.clone())) {
        Ok((engine, unexpected)) => build(app, paths, engine, unexpected, settings_error),
        Err(e) => database_error(app, paths, settings, &e.to_string()),
    }
}

/// Shown instead of the main window when the database cannot be opened. The file is never
/// modified; restoring moves it aside and rebuilds from the newest automatic backup.
fn database_error(app: &adw::Application, paths: Paths, settings: Settings, error: &str) {
    let backups = backup::list_auto_backups(&paths.backups);
    let status = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Could Not Open the Database")
        .description(glib::markup_escape_text(&format!(
            "{}\n\n{error}\n\nThe file has not been changed.",
            paths.db.display()
        )))
        .build();
    let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
    let quit = gtk::Button::builder().label("_Quit").use_underline(true).css_classes(["pill"]).build();
    buttons.append(&quit);
    let restore = gtk::Button::builder()
        .label("_Restore Newest Backup")
        .use_underline(true)
        .css_classes(["pill", "suggested-action"])
        .visible(!backups.is_empty())
        .build();
    buttons.append(&restore);
    status.set_child(Some(&buttons));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));
    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("Pomodoro")
        .default_width(420)
        .default_height(520)
        .content(&toolbar)
        .build();
    quit.connect_clicked(glib::clone!(#[weak] win, move |_| win.close()));

    let paths = RefCell::new(Some(paths));
    restore.connect_clicked(glib::clone!(
        #[weak]
        win,
        #[weak]
        app,
        move |button| {
            let Some(paths) = paths.borrow_mut().take() else { return };
            let Some(newest) = backups.first() else { return };
            match restore_from_backup(&paths, newest, &settings) {
                Ok(engine) => {
                    build(&app, paths, engine, false, Some("Restored from the newest automatic backup".into()));
                    win.destroy();
                }
                Err(e) => {
                    status.set_description(Some(&glib::markup_escape_text(&format!("Restore failed: {e}"))));
                    button.set_visible(false);
                }
            }
        }
    ));
    win.present();
}

fn restore_from_backup(paths: &Paths, backup_file: &std::path::Path, settings: &Settings) -> Result<Engine, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    for suffix in ["", "-wal", "-shm"] {
        let from = std::path::PathBuf::from(format!("{}{suffix}", paths.db.display()));
        if from.exists() {
            let to = format!("{}.unreadable-{stamp}{suffix}", paths.db.display());
            std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    let text = std::fs::read_to_string(backup_file).map_err(|e| e.to_string())?;
    let mut db = Db::open(&paths.db).map_err(|e| e.to_string())?;
    let b = backup::parse(&text, &db).map_err(|e| e.to_string())?;
    backup::apply(&mut db, &b, backup::ImportMode::Replace).map_err(|e| e.to_string())?;
    let (engine, _) = Engine::new(db, Box::new(SystemClock::new()), settings.clone()).map_err(|e| e.to_string())?;
    Ok(engine)
}

pub fn run_auto_backup(ctx: &Ctx) {
    let e = ctx.engine.borrow();
    if !e.settings().auto_backup {
        return;
    }
    let r = backup::auto_backup(&ctx.paths.backups, e.db(), e.settings(), e.now(), e.today_date());
    drop(e);
    if let Err(err) = r {
        ctx.toast(&format!("Automatic backup failed: {err}"));
    }
}

struct WinState {
    timer: Rc<TimerView>,
    /// Views hold only weak references to themselves in signal handlers; this keeps them alive.
    _stats: Rc<StatsView>,
    tick: Cell<Option<glib::SourceId>>,
    paused_for_sleep: Cell<bool>,
    last_date: Cell<chrono::NaiveDate>,
}

fn build(app: &adw::Application, paths: Paths, engine: Engine, unexpected: bool, notice: Option<String>) {
    let ctx = Ctx::new(app, paths, engine);

    let timer = TimerView::new(&ctx);
    let stats = StatsView::new(&ctx);

    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(&timer.widget, Some("timer"), "Timer", "preferences-system-time-symbolic");
    stack.add_titled_with_icon(&stats.widget, Some("stats"), "Statistics", "view-grid-symbolic");

    let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    let menu = gio::Menu::new();
    let main = gio::Menu::new();
    main.append(Some("_Preferences"), Some("win.preferences"));
    main.append(Some("_Export Data…"), Some("win.export"));
    main.append(Some("_Import Data…"), Some("win.import"));
    menu.append_section(None, &main);
    let help = gio::Menu::new();
    help.append(Some("_Keyboard Shortcuts"), Some("win.show-help-overlay"));
    help.append(Some("_About Pomodoro"), Some("win.about"));
    menu.append_section(None, &help);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text("Main Menu")
        .build();
    header.pack_end(&menu_button);

    let banner = adw::Banner::builder().title(CRASH_NOTICE).button_label("_Dismiss").revealed(unexpected).build();
    banner.connect_button_clicked(|b| b.set_revealed(false));

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&banner);
    toolbar.set_content(Some(&toasts));
    toolbar.add_bottom_bar(&switcher_bar);

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("Pomodoro")
        .default_width(360)
        .default_height(560)
        .width_request(300)
        .height_request(420)
        .content(&toolbar)
        .build();

    // Narrow: move the view switcher to the bottom. Very narrow: also shrink the ring.
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 550sp").expect("condition"));
    narrow.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
    narrow.add_setter(&header, "title-widget", Some(&None::<gtk::Widget>.to_value()));
    let tiny = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 340sp").expect("condition"));
    tiny.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
    tiny.add_setter(&header, "title-widget", Some(&None::<gtk::Widget>.to_value()));
    for area in timer.ring_areas() {
        tiny.add_setter(area, "content-width", Some(&200.to_value()));
        tiny.add_setter(area, "content-height", Some(&200.to_value()));
    }
    win.add_breakpoint(narrow);
    win.add_breakpoint(tiny);

    let _ = ctx.window.set(win.clone());
    let _ = ctx.toasts.set(toasts);
    if let Some(n) = notice {
        ctx.toast(&n);
    }

    let state = Rc::new(WinState {
        timer: timer.clone(),
        _stats: stats.clone(),
        tick: Cell::new(None),
        paused_for_sleep: Cell::new(false),
        last_date: Cell::new(ctx.engine.borrow().today_date()),
    });

    add_actions(&ctx, &win, &timer);
    add_shortcuts(&win);
    win.set_help_overlay(Some(&shortcuts_window()));

    // Every change refreshes the title, action states and the tick schedule.
    {
        let (c, s, w) = (Rc::downgrade(&ctx), state.clone(), win.downgrade());
        ctx.subscribe(move |_| {
            if let (Some(ctx), Some(win)) = (c.upgrade(), w.upgrade()) {
                refresh_window(&ctx, &win);
                schedule_tick(&ctx, &s);
            }
        });
    }

    win.connect_close_request(glib::clone!(
        #[strong]
        ctx,
        #[strong]
        state,
        move |_| {
            if let Some(id) = state.tick.take() {
                id.remove();
            }
            let r = ctx.engine.borrow_mut().close();
            ctx.report(r);
            glib::Propagation::Proceed
        }
    ));

    // Suspend pauses the timer; waking up shows a toast.
    let sleep = suspend::watch(glib::clone!(
        #[weak]
        ctx,
        #[strong]
        state,
        move |sleeping| {
            if sleeping {
                let r = ctx.engine.borrow_mut().pause();
                if ctx.report(r) == Some(true) {
                    state.paused_for_sleep.set(true);
                    ctx.notify(Change::Timer);
                }
            } else if state.paused_for_sleep.replace(false) {
                ctx.toast("Timer paused while the computer was asleep");
                ctx.notify(Change::Timer);
            }
        }
    ));
    win.connect_destroy(move |_| {
        sleep.borrow_mut().take();
    });

    // Refresh "today" figures after midnight (or the configured day start) while idle.
    glib::timeout_add_seconds_local(
        60,
        glib::clone!(
            #[weak]
            ctx,
            #[strong]
            state,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                let today = ctx.engine.borrow().today_date();
                if state.last_date.replace(today) != today {
                    let r = ctx.engine.borrow_mut().ensure_goal();
                    ctx.report(r);
                    run_auto_backup(&ctx);
                    ctx.notify(Change::Data);
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    run_auto_backup(&ctx);
    refresh_window(&ctx, &win);
    schedule_tick(&ctx, &state);
    win.present();
}

fn refresh_window(ctx: &Ctx, win: &adw::ApplicationWindow) {
    let e = ctx.engine.borrow();
    let snap = e.snapshot();
    win.set_title(Some(&window_title(&snap)));
    let enable = |name: &str, on: bool| {
        if let Some(a) = win.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            a.set_enabled(on);
        }
    };
    enable("cancel-pomodoro", snap.in_focus());
    enable("cancel-session", snap.session.is_some());
    enable("skip-break", e.can_skip_break());
}

/// Wakes up when the displayed second changes, and only while a phase runs.
fn schedule_tick(ctx: &Rc<Ctx>, state: &Rc<WinState>) {
    if let Some(id) = state.tick.take() {
        id.remove();
    }
    let Some(remaining) = ctx.engine.borrow().remaining_sec() else { return };
    let frac = remaining - remaining.floor();
    let delay = if frac < 0.005 { 1.0 } else { frac } + 0.005;
    let (c, s) = (Rc::downgrade(ctx), state.clone());
    let id = glib::timeout_add_local_once(Duration::from_secs_f64(delay.min(1.0)), move || {
        // This source is finished; forget its id so it is not removed twice.
        let _ = s.tick.take();
        let Some(ctx) = c.upgrade() else { return };
        let r = ctx.engine.borrow_mut().tick();
        match ctx.report(r) {
            Some(Some(completion)) => {
                on_completion(&ctx, &s.timer, &completion);
                ctx.notify(Change::Data);
            }
            _ => ctx.notify(Change::Timer),
        }
    });
    state.tick.set(Some(id));
}

fn on_completion(ctx: &Rc<Ctx>, timer: &Rc<TimerView>, c: &Completion) {
    let settings = ctx.settings();
    let chime = if c.finished == PhaseKind::Focus { Chime::FocusEnd } else { Chime::BreakEnd };
    ctx.sound.play(chime, &settings);
    if c.goal_reached {
        timer.flash_goal_reached();
    }
    if c.session_completed {
        run_auto_backup(ctx);
    }
    if settings.notifications {
        let (title, body) = notification_text(c, ctx);
        let n = gio::Notification::new(&title);
        if !body.is_empty() {
            n.set_body(Some(&body));
        }
        n.set_default_action("app.raise");
        ctx.app.send_notification(Some("phase"), &n);
    }
}

fn notification_text(c: &Completion, ctx: &Ctx) -> (String, String) {
    let mins = |sec: u32| sec / 60;
    let mut body = Vec::new();
    let title = match (c.finished, c.next) {
        (PhaseKind::Focus, Some(PhaseKind::Focus)) if c.mode == Mode::Blitz => {
            format!("Pomodoro {} of {} done — next one started", c.completed_in_session, c.target)
        }
        (PhaseKind::Focus, Some(PhaseKind::LongBreak)) => {
            if c.next_started {
                format!("Session complete — {} min long break started", mins(c.next_planned_sec))
            } else {
                format!("Session complete — take a {} min break", mins(c.next_planned_sec))
            }
        }
        (PhaseKind::Focus, _) => {
            body.push(format!("Pomodoro {} of {} done", c.completed_in_session, c.target));
            if c.next_started {
                format!("Focus complete — {} min break started", mins(c.next_planned_sec))
            } else {
                format!("Focus complete — take a {} min break", mins(c.next_planned_sec))
            }
        }
        (PhaseKind::ShortBreak, _) if c.next_started => "Break over — focus started".to_owned(),
        (PhaseKind::ShortBreak, _) => "Break over — ready to focus".to_owned(),
        (PhaseKind::LongBreak, _) => "Long break over — ready for a new session".to_owned(),
    };
    if c.goal_reached {
        if let Some(t) = ctx.report(ctx.engine.borrow().today()) {
            body.push(format!("Daily goal reached: {} / {} sessions", t.completed_sessions, t.goal_sessions));
        }
    }
    (title, body.join("\n"))
}

fn add_actions(ctx: &Rc<Ctx>, win: &adw::ApplicationWindow, timer: &Rc<TimerView>) {
    let action = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        win.add_action(&a);
    };
    let c = ctx.clone();
    let t = timer.clone();
    action(
        "toggle",
        Box::new(move || {
            let (tag, mode) = t.current_selection();
            let r = c.engine.borrow_mut().toggle(tag, mode);
            c.report(r);
            c.notify(Change::Timer);
        }),
    );
    let c = ctx.clone();
    action("cancel-pomodoro", Box::new(move || dialogs::cancel_pomodoro(&c)));
    let c = ctx.clone();
    action("cancel-session", Box::new(move || dialogs::cancel_session(&c)));
    let c = ctx.clone();
    action(
        "skip-break",
        Box::new(move || {
            let r = c.engine.borrow_mut().skip_break();
            c.report(r);
            c.notify(Change::Timer);
        }),
    );
    let c = ctx.clone();
    action("preferences", Box::new(move || prefs::show(&c)));
    let c = ctx.clone();
    action("export", Box::new(move || dialogs::export_json(&c)));
    let c = ctx.clone();
    action("import", Box::new(move || dialogs::import(&c)));
    let c = ctx.clone();
    action("about", Box::new(move || dialogs::about(&c)));
    ctx.app.set_accels_for_action("win.preferences", &["<Control>comma"]);
}

/// Timer shortcuts. Handled in the capture phase so Space works wherever focus is, but never
/// while typing in a text field or while a dialog is open.
fn add_shortcuts(win: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        win,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, mods| {
            if win.visible_dialog().is_some() {
                return glib::Propagation::Proceed;
            }
            let typing = gtk::prelude::GtkWindowExt::focus(&win)
                .is_some_and(|w| w.is::<gtk::Text>() || w.is::<gtk::Editable>());
            if typing {
                return glib::Propagation::Proceed;
            }
            let mods = mods & (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK);
            let ctrl = gdk::ModifierType::CONTROL_MASK;
            let action = match (key, mods) {
                (gdk::Key::space, m) if m.is_empty() => "toggle",
                (gdk::Key::BackSpace, m) if m == ctrl => "cancel-pomodoro",
                (gdk::Key::BackSpace, m) if m == ctrl | gdk::ModifierType::SHIFT_MASK => "cancel-session",
                (gdk::Key::n, m) if m.is_empty() => "skip-break",
                _ => return glib::Propagation::Proceed,
            };
            match win.lookup_action(action) {
                Some(a) if a.is_enabled() => {
                    a.activate(None);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    win.add_controller(keys);
}

fn shortcuts_window() -> gtk::ShortcutsWindow {
    const UI: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="help_overlay">
    <property name="modal">True</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">shortcuts</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Timer</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Start, pause or resume</property><property name="accelerator">space</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Cancel pomodoro</property><property name="accelerator">&lt;Control&gt;BackSpace</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Cancel session</property><property name="accelerator">&lt;Control&gt;&lt;Shift&gt;BackSpace</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Skip break</property><property name="accelerator">n</property></object></child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">General</property>
            <child><object class="GtkShortcutsShortcut"><property name="title">Preferences</property><property name="accelerator">&lt;Control&gt;comma</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Keyboard shortcuts</property><property name="accelerator">&lt;Control&gt;question</property></object></child>
            <child><object class="GtkShortcutsShortcut"><property name="title">Quit</property><property name="accelerator">&lt;Control&gt;q</property></object></child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>"#;
    gtk::Builder::from_string(UI).object("help_overlay").expect("shortcuts window")
}
