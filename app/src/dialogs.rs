//! Confirmation, tag, export/import and about dialogs.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use pomodoro_core::backup::{self, ImportMode};
use pomodoro_core::db::validate_tag_name;
use pomodoro_core::model::{Tag, TagColor};
use pomodoro_core::time::{fmt_decimal, fmt_minutes};
use pomodoro_core::APP_ID;

use crate::ctx::{label_widget, tag_dot, Change, Ctx};

fn alert(heading: &str, body: &str) -> adw::AlertDialog {
    adw::AlertDialog::builder().heading(heading).body(body).build()
}

pub fn cancel_pomodoro(ctx: &Rc<Ctx>) {
    let Some((elapsed, fraction)) = ctx.engine.borrow().cancel_pomodoro_preview() else { return };
    let body = if elapsed > 0.0 {
        format!(
            "{} of focus ({} pomodoro) will be saved as incomplete.",
            fmt_minutes(elapsed),
            fmt_decimal(fraction)
        )
    } else {
        "No focus time has passed yet, so nothing will be saved.".to_owned()
    };
    let d = alert("Cancel this pomodoro?", &body);
    d.add_responses(&[("keep", "_Keep Going"), ("cancel", "_Cancel Pomodoro")]);
    d.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("keep"));
    d.set_close_response("keep");
    let ctx = ctx.clone();
    d.clone().choose(Some(ctx.clone().window()), None::<&gio::Cancellable>, move |r| {
        if r == "cancel" {
            let r = ctx.engine.borrow_mut().cancel_pomodoro();
            ctx.report(r);
            ctx.notify(Change::Data);
        }
    });
}

pub fn cancel_session(ctx: &Rc<Ctx>) {
    let Some((equivalents, fraction)) = ctx.engine.borrow().cancel_session_preview() else { return };
    let body = if equivalents > 0.0 {
        format!(
            "{} pomodoro{} of progress ({} session) will be saved as incomplete.",
            fmt_decimal(equivalents),
            if fmt_decimal(equivalents) == "1" { "" } else { "s" },
            fmt_decimal(fraction)
        )
    } else {
        "No progress has been made yet, so nothing will be saved.".to_owned()
    };
    let d = alert("Cancel this session?", &body);
    d.add_responses(&[("keep", "_Keep Going"), ("cancel", "_Cancel Session")]);
    d.set_response_appearance("cancel", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("keep"));
    d.set_close_response("keep");
    let ctx = ctx.clone();
    d.clone().choose(Some(ctx.clone().window()), None::<&gio::Cancellable>, move |r| {
        if r == "cancel" {
            let r = ctx.engine.borrow_mut().cancel_session();
            ctx.report(r);
            ctx.notify(Change::Data);
        }
    });
}

/// Name entry plus a row of colour swatches.
fn tag_editor(name: &str, color: TagColor) -> (gtk::Box, gtk::Entry, Rc<Cell<TagColor>>) {
    let chosen = Rc::new(Cell::new(color));
    let entry = gtk::Entry::builder()
        .text(name)
        .placeholder_text("Name, e.g. Chemistry")
        .max_length(32)
        .activates_default(true)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label("Tag name")]);
    let swatches = gtk::Box::builder().spacing(4).halign(gtk::Align::Center).build();
    let mut group: Option<gtk::ToggleButton> = None;
    for &c in TagColor::ALL {
        let b = gtk::ToggleButton::builder()
            .child(&{
                let d = tag_dot(Some(c));
                d.add_css_class("large");
                d
            })
            .css_classes(["flat", "circular"])
            .active(c == color)
            .build();
        label_widget(&b, c.label());
        if let Some(g) = &group {
            b.set_group(Some(g));
        } else {
            group = Some(b.clone());
        }
        let chosen = chosen.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                chosen.set(c);
            }
        });
        swatches.append(&b);
    }
    let bx = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    bx.append(&entry);
    bx.append(&swatches);
    (bx, entry, chosen)
}

fn tag_dialog(
    ctx: &Rc<Ctx>,
    heading: &str,
    action: &str,
    tag: Option<&Tag>,
    on_ok: impl Fn(&str, TagColor) -> bool + 'static,
) {
    let d = alert(heading, "");
    let (editor, entry, color) = tag_editor(
        tag.map(|t| t.name.as_str()).unwrap_or(""),
        tag.map(|t| t.color).unwrap_or(TagColor::Blue),
    );
    d.set_extra_child(Some(&editor));
    d.add_responses(&[("cancel", "_Cancel"), ("ok", action)]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    let valid = |e: &gtk::Entry| validate_tag_name(&e.text()).is_ok();
    d.set_response_enabled("ok", valid(&entry));
    entry.connect_changed(glib::clone!(
        #[weak]
        d,
        move |e| d.set_response_enabled("ok", valid(e))
    ));
    d.connect_response(Some("ok"), move |_, _| {
        on_ok(&entry.text(), color.get());
    });
    d.present(Some(ctx.window()));
    editor.first_child().map(|e| e.grab_focus());
}

/// Asks for a name and colour, creates the tag, and passes it to `on_created`.
pub fn new_tag(ctx: &Rc<Ctx>, on_created: impl Fn(Tag) + 'static) {
    let c = ctx.clone();
    tag_dialog(ctx, "New Tag", "_Create", None, move |name, color| {
        let r = c.engine.borrow_mut().db_mut().create_tag(name, color);
        match c.report(r) {
            Some(tag) => {
                on_created(tag);
                true
            }
            None => false,
        }
    });
}

pub fn edit_tag(ctx: &Rc<Ctx>, tag: &Tag) {
    let c = ctx.clone();
    let id = tag.id.clone();
    tag_dialog(ctx, "Edit Tag", "_Save", Some(tag), move |name, color| {
        let r = {
            let mut e = c.engine.borrow_mut();
            e.db_mut().rename_tag(&id, name).and_then(|_| e.db_mut().recolor_tag(&id, color))
        };
        let ok = c.report(r).is_some();
        c.notify(Change::Tags);
        ok
    });
}

// ---- export / import -------------------------------------------------------

fn json_filter() -> gio::ListStore {
    let f = gtk::FileFilter::new();
    f.set_name(Some("Pomodoro backup (JSON)"));
    f.add_suffix("json");
    f.add_mime_type("application/json");
    let store = gio::ListStore::new::<gtk::FileFilter>();
    store.append(&f);
    store
}

fn save_to_file(ctx: &Rc<Ctx>, name: String, contents: impl FnOnce() -> Option<String> + 'static) {
    let dialog = gtk::FileDialog::builder().title("Export Data").initial_name(name).modal(true).build();
    let ctx = ctx.clone();
    dialog.save(Some(ctx.clone().window()), None::<&gio::Cancellable>, move |file| {
        let Ok(file) = file else { return };
        let Some(text) = contents() else { return };
        let r = file.replace_contents(
            text.as_bytes(),
            None,
            false,
            gio::FileCreateFlags::REPLACE_DESTINATION,
            None::<&gio::Cancellable>,
        );
        if ctx.report(r).is_some() {
            ctx.toast("Export saved");
        }
    });
}

pub fn export_json(ctx: &Rc<Ctx>) {
    let today = ctx.engine.borrow().today_date();
    let c = ctx.clone();
    save_to_file(ctx, backup::default_file_name(today), move || {
        let e = c.engine.borrow();
        let b = backup::export(e.db(), e.settings(), e.now());
        c.report(b).map(|b| backup::to_json(&b))
    });
}

pub fn export_csv(ctx: &Rc<Ctx>) {
    let today = ctx.engine.borrow().today_date();
    let c = ctx.clone();
    save_to_file(ctx, format!("pomodoro-{today}.csv"), move || {
        let csv = backup::to_csv(c.engine.borrow().db());
        c.report(csv)
    });
}

pub fn import(ctx: &Rc<Ctx>) {
    let dialog = gtk::FileDialog::builder().title("Import Data").filters(&json_filter()).modal(true).build();
    let ctx = ctx.clone();
    dialog.open(Some(ctx.clone().window()), None::<&gio::Cancellable>, move |file| {
        let Ok(file) = file else { return };
        let text = match file.load_contents(None::<&gio::Cancellable>) {
            Ok((bytes, _)) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => return ctx.toast(&format!("Import failed: {e}")),
        };
        let parsed = backup::parse(&text, ctx.engine.borrow().db());
        match parsed {
            Ok(b) => import_preview(&ctx, b),
            Err(e) => ctx.toast(&format!("Import failed: {e}")),
        }
    });
}

fn import_preview(ctx: &Rc<Ctx>, b: backup::Backup) {
    let p = backup::preview(&b);
    let range = match (p.first, p.last) {
        (Some(a), Some(z)) if a == z => format!(" from {a}"),
        (Some(a), Some(z)) => format!(" from {a} to {z}"),
        _ => String::new(),
    };
    let body = format!(
        "{} pomodoros, {} sessions and {} tags{range}.",
        p.pomodoros, p.sessions, p.tags
    );
    let d = alert("Import Backup", &body);

    let session_active = ctx.engine.borrow().snapshot().session.is_some();
    let merge = gtk::CheckButton::builder().label("Merge: add records that are not present").active(true).build();
    let replace = gtk::CheckButton::builder().label("Replace all current history").group(&merge).build();
    if session_active {
        replace.set_sensitive(false);
        replace.set_tooltip_text(Some("Cancel the active session first"));
    }
    let settings = gtk::CheckButton::builder().label("Also import settings").build();
    replace.connect_toggled(glib::clone!(
        #[weak]
        settings,
        move |r| {
            // Replace always restores settings.
            settings.set_active(r.is_active());
            settings.set_sensitive(!r.is_active());
        }
    ));
    let bx = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    bx.append(&merge);
    bx.append(&replace);
    bx.append(&settings);
    d.set_extra_child(Some(&bx));
    d.add_responses(&[("cancel", "_Cancel"), ("import", "_Import")]);
    d.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    d.set_close_response("cancel");

    let ctx = ctx.clone();
    d.clone().choose(Some(ctx.clone().window()), None::<&gio::Cancellable>, move |r| {
        if r != "import" {
            return;
        }
        let with_settings = settings.is_active();
        if !replace.is_active() {
            apply_import(&ctx, &b, ImportMode::Merge, with_settings);
            return;
        }
        let confirm = alert(
            "Replace all history?",
            "Your current tags, sessions and pomodoros will be deleted and replaced by the backup.",
        );
        confirm.add_responses(&[("cancel", "_Cancel"), ("replace", "_Replace")]);
        confirm.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        confirm.set_close_response("cancel");
        let c = ctx.clone();
        confirm.clone().choose(Some(ctx.window()), None::<&gio::Cancellable>, move |r| {
            if r == "replace" {
                apply_import(&c, &b, ImportMode::Replace, true);
            }
        });
    });
}

fn apply_import(ctx: &Rc<Ctx>, b: &backup::Backup, mode: ImportMode, with_settings: bool) {
    if mode == ImportMode::Replace && ctx.engine.borrow().snapshot().session.is_some() {
        ctx.toast("Cancel the active session before replacing history");
        return;
    }
    let r = backup::apply(ctx.engine.borrow_mut().db_mut(), b, mode);
    let Some(result) = ctx.report(r) else { return };
    if with_settings {
        let imported = b.settings.clone();
        ctx.update_settings(|s| *s = imported);
    }
    let r = ctx.engine.borrow_mut().reload();
    ctx.report(r);
    ctx.notify(Change::Tags);
    ctx.notify(Change::Data);
    ctx.toast(&format!(
        "Imported {} sessions and {} pomodoros",
        result.sessions, result.pomodoros
    ));
}

pub fn open_data_folder(ctx: &Rc<Ctx>) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&ctx.paths.data_dir)));
    let c = ctx.clone();
    launcher.launch(Some(ctx.window()), None::<&gio::Cancellable>, move |r| {
        c.report(r);
    });
}

pub fn about(ctx: &Rc<Ctx>) {
    let d = adw::AboutDialog::builder()
        .application_name("Pomodoro")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("0xEyeball")
        .license_type(gtk::License::Gpl30)
        .website("https://github.com/0xEyeball/pomodoro")
        .issue_url("https://github.com/0xEyeball/pomodoro/issues")
        .build();
    d.present(Some(ctx.window()));
}
