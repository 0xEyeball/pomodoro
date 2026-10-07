//! Preferences dialog.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use pomodoro_core::model::Mode;
use pomodoro_core::settings::*;

use crate::ctx::{label_widget, tag_dot, Change, Ctx};
use crate::dialogs;

fn spin(title: &str, subtitle: &str, (lo, hi): (u32, u32), value: u32) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(lo as f64, hi as f64, 1.0);
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.set_value(value as f64);
    row
}

fn switch(title: &str, subtitle: &str, value: bool) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(value).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

fn button_row(title: &str, icon: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name(icon));
    row
}

pub fn show(ctx: &Rc<Ctx>) {
    let s = ctx.settings();
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(true);

    // ---- Timer page -------------------------------------------------------
    let timer = adw::PreferencesPage::builder().title("Timer").icon_name("preferences-system-time-symbolic").build();

    let g = adw::PreferencesGroup::builder()
        .title("Timer")
        .description("Changes apply from the next phase")
        .build();
    let focus = spin("Focus", "Minutes", FOCUS_RANGE, s.focus_min);
    let short = spin("Short Break", "Minutes", SHORT_BREAK_RANGE, s.short_break_min);
    let long = spin("Long Break", "Minutes", LONG_BREAK_RANGE, s.long_break_min);
    let per = spin("Pomodoros per Session", "", POMODOROS_RANGE, s.pomodoros_per_session);
    let auto = switch("Auto-start Next Phase", "Blitz sessions always start the next focus", s.auto_start);
    for w in [&focus, &short, &long, &per] {
        g.add(w);
    }
    g.add(&auto);
    timer.add(&g);

    let bind_spin = |row: &adw::SpinRow, f: fn(&mut Settings, u32)| {
        let ctx = ctx.clone();
        row.connect_value_notify(move |r| {
            let v = r.value() as u32;
            ctx.update_settings(|s| f(s, v));
        });
    };
    bind_spin(&focus, |s, v| s.focus_min = v);
    bind_spin(&short, |s, v| s.short_break_min = v);
    bind_spin(&long, |s, v| s.long_break_min = v);
    bind_spin(&per, |s, v| s.pomodoros_per_session = v);
    let bind_switch = |row: &adw::SwitchRow, f: fn(&mut Settings, bool)| {
        let ctx = ctx.clone();
        row.connect_active_notify(move |r| {
            let v = r.is_active();
            ctx.update_settings(|s| f(s, v));
        });
    };
    bind_switch(&auto, |s, v| s.auto_start = v);

    let g = adw::PreferencesGroup::builder().title("Session").build();
    let mode = adw::ComboRow::builder()
        .title("Default Mode")
        .subtitle("Blitz skips short breaks")
        .model(&gtk::StringList::new(&["Standard", "Blitz"]))
        .selected(if s.default_mode == Mode::Blitz { 1 } else { 0 })
        .build();
    let remember = switch("Remember Last Tag", "", s.remember_last_tag);
    g.add(&mode);
    g.add(&remember);
    timer.add(&g);
    mode.connect_selected_notify(glib::clone!(
        #[strong]
        ctx,
        move |r| {
            let m = if r.selected() == 1 { Mode::Blitz } else { Mode::Standard };
            ctx.update_settings(|s| s.default_mode = m);
            if ctx.engine.borrow().snapshot().session.is_none() {
                *ctx.next_mode.borrow_mut() = m;
                ctx.notify(Change::Timer);
            }
        }
    ));
    bind_switch(&remember, |s, v| s.remember_last_tag = v);

    let g = adw::PreferencesGroup::builder().title("Daily Goal").build();
    let goal = spin("Sessions per Day", "Changes apply from today; past days keep their goal", GOAL_RANGE, s.goal_sessions);
    let day_start = spin("Day Starts At", "Hour of the day (0 = midnight)", (0, 23), s.day_start_hour);
    g.add(&goal);
    g.add(&day_start);
    timer.add(&g);
    bind_spin(&goal, |s, v| s.goal_sessions = v);
    bind_spin(&day_start, |s, v| s.day_start_hour = v);
    dialog.add(&timer);

    // ---- Tags page --------------------------------------------------------
    let tags_page = adw::PreferencesPage::builder().title("Tags").icon_name("bookmark-new-symbolic").build();
    let active = adw::PreferencesGroup::builder().title("Tags").build();
    let add = gtk::Button::builder().icon_name("list-add-symbolic").css_classes(["flat"]).valign(gtk::Align::Center).build();
    label_widget(&add, "New Tag");
    active.set_header_suffix(Some(&add));
    let archived = adw::PreferencesGroup::builder()
        .title("Archived")
        .description("Archived tags are hidden from the picker but kept in history")
        .build();
    tags_page.add(&active);
    tags_page.add(&archived);
    dialog.add(&tags_page);
    let rows: Rc<std::cell::RefCell<Vec<(adw::PreferencesGroup, adw::ActionRow)>>> = Rc::default();
    let rebuild = Rc::new(glib::clone!(
        #[strong]
        ctx,
        #[weak]
        active,
        #[weak]
        archived,
        #[strong]
        rows,
        move || {
            for (g, r) in rows.borrow_mut().drain(..) {
                g.remove(&r);
            }
            let tags = ctx.all_tags();
            for t in tags {
                let group = if t.archived { &archived } else { &active };
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&t.name)).build();
                row.add_prefix(&tag_dot(Some(t.color)));
                let icon_button = |icon: &str, label: &str| {
                    let b = gtk::Button::builder().icon_name(icon).valign(gtk::Align::Center).css_classes(["flat"]).build();
                    label_widget(&b, label);
                    row.add_suffix(&b);
                    b
                };
                if t.archived {
                    let restore = icon_button("edit-undo-symbolic", "Restore");
                    let (c, id) = (ctx.clone(), t.id.clone());
                    restore.connect_clicked(move |_| {
                        let r = c.engine.borrow_mut().db_mut().set_tag_archived(&id, false);
                        c.report(r);
                        c.notify(Change::Tags);
                    });
                } else {
                    let edit = icon_button("document-edit-symbolic", "Rename or Recolour");
                    let (c, tag) = (ctx.clone(), t.clone());
                    edit.connect_clicked(move |_| dialogs::edit_tag(&c, &tag));
                    let archive = icon_button("user-trash-symbolic", "Delete (Archive)");
                    let (c, id) = (ctx.clone(), t.id.clone());
                    archive.connect_clicked(move |_| {
                        let r = c.engine.borrow_mut().db_mut().set_tag_archived(&id, true);
                        c.report(r);
                        c.notify(Change::Tags);
                    });
                }
                group.add(&row);
                rows.borrow_mut().push((group.clone(), row));
            }
            archived.set_visible(rows.borrow().iter().any(|(g, _)| g == &archived));
        }
    ));
    rebuild();
    {
        let rebuild = Rc::downgrade(&rebuild);
        ctx.subscribe(move |change| {
            if change == Change::Tags {
                if let Some(r) = rebuild.upgrade() {
                    r();
                }
            }
        });
    }
    add.connect_clicked(glib::clone!(
        #[strong]
        ctx,
        move |_| dialogs::new_tag(&ctx, glib::clone!(#[strong] ctx, move |_| ctx.notify(Change::Tags)))
    ));
    // Keep the rebuild closure alive as long as the dialog (the listener only holds a weak ref).
    dialog.connect_closed(move |_| {
        let _ = &rebuild;
    });

    // ---- General page -----------------------------------------------------
    let general = adw::PreferencesPage::builder().title("General").icon_name("preferences-system-symbolic").build();

    let g = adw::PreferencesGroup::builder().title("Sound").build();
    let sound = switch("Chime", "Play a soft chime when a phase ends", s.sound_enabled);
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
    volume.set_value(s.volume);
    volume.set_hexpand(true);
    volume.set_valign(gtk::Align::Center);
    volume.set_width_request(140);
    volume.update_property(&[gtk::accessible::Property::Label("Volume")]);
    let volume_row = adw::ActionRow::builder().title("Volume").build();
    volume_row.add_suffix(&volume);
    let test = button_row("Test Sound", "media-playback-start-symbolic");
    let low = switch("Lower Chime for Break End", "", s.separate_break_sound);
    for w in [sound.upcast_ref::<gtk::Widget>(), volume_row.upcast_ref(), test.upcast_ref(), low.upcast_ref()] {
        g.add(w);
    }
    general.add(&g);
    bind_switch(&sound, |s, v| s.sound_enabled = v);
    bind_switch(&low, |s, v| s.separate_break_sound = v);
    volume.connect_value_changed(glib::clone!(
        #[strong]
        ctx,
        move |v| {
            let value = (v.value() * 100.0).round() / 100.0;
            ctx.update_settings(|s| s.volume = value);
        }
    ));
    test.connect_activated(glib::clone!(
        #[strong]
        ctx,
        move |_| {
            let s = ctx.settings();
            ctx.sound.play_at(false, s.volume);
        }
    ));

    let g = adw::PreferencesGroup::builder().title("Notifications").build();
    let notify = switch("Desktop Notifications", "Shown when a phase ends", s.notifications);
    g.add(&notify);
    general.add(&g);
    bind_switch(&notify, |s, v| s.notifications = v);

    let g = adw::PreferencesGroup::builder().title("Appearance").build();
    let appearance = adw::ComboRow::builder()
        .title("Style")
        .model(&gtk::StringList::new(&["Follow System", "Light", "Dark"]))
        .selected(match s.appearance {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        })
        .build();
    g.add(&appearance);
    general.add(&g);
    appearance.connect_selected_notify(glib::clone!(
        #[strong]
        ctx,
        move |r| {
            let a = match r.selected() {
                1 => Appearance::Light,
                2 => Appearance::Dark,
                _ => Appearance::System,
            };
            ctx.update_settings(|s| s.appearance = a);
        }
    ));

    let g = adw::PreferencesGroup::builder().title("Data").build();
    let export = button_row("Export Data…", "document-save-symbolic");
    let csv = button_row("Export as CSV…", "x-office-spreadsheet-symbolic");
    let import = button_row("Import Data…", "document-open-symbolic");
    let folder = button_row("Open Data Folder", "folder-open-symbolic");
    let backup = switch("Automatic Backups", "Keep the last 7 daily snapshots in the data folder", s.auto_backup);
    for w in [export.upcast_ref::<gtk::Widget>(), csv.upcast_ref(), import.upcast_ref(), folder.upcast_ref(), backup.upcast_ref()] {
        g.add(w);
    }
    general.add(&g);
    export.connect_activated(glib::clone!(#[strong] ctx, move |_| dialogs::export_json(&ctx)));
    csv.connect_activated(glib::clone!(#[strong] ctx, move |_| dialogs::export_csv(&ctx)));
    import.connect_activated(glib::clone!(#[strong] ctx, move |_| dialogs::import(&ctx)));
    folder.connect_activated(glib::clone!(#[strong] ctx, move |_| dialogs::open_data_folder(&ctx)));
    bind_switch(&backup, |s, v| s.auto_backup = v);
    backup.connect_active_notify(glib::clone!(
        #[strong]
        ctx,
        move |r| {
            if r.is_active() {
                crate::window::run_auto_backup(&ctx);
            }
        }
    ));
    dialog.add(&general);

    dialog.present(Some(ctx.window()));
}
