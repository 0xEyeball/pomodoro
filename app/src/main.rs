mod ctx;
mod dialogs;
mod prefs;
mod ring;
mod sound;
mod stats_view;
mod suspend;
mod timer_view;
mod week;
mod window;

use adw::prelude::*;
use gtk::{gio, glib};
use pomodoro_core::APP_ID;

fn main() -> glib::ExitCode {
    // Makes the X11 WM_CLASS and Wayland app_id match the .desktop file.
    glib::set_prgname(Some(APP_ID));
    glib::set_application_name("Pomodoro");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::empty())
        .build();

    app.connect_startup(|_| {
        ctx::load_css();
    });

    app.connect_activate(|app| {
        // Single instance: a second launch lands here in the primary instance and just raises it.
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        window::open(app);
    });

    let raise = gio::SimpleAction::new("raise", None);
    raise.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            if let Some(win) = app.active_window() {
                win.present();
            } else {
                app.activate();
            }
        }
    ));
    app.add_action(&raise);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            // Closing the windows runs the normal pause-and-save path.
            for w in app.windows() {
                w.close();
            }
        }
    ));
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    app.run()
}
