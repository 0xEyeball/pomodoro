//! Watches logind's `PrepareForSleep` signal on the system bus.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;

/// Calls `on_change(true)` right before the system suspends and `on_change(false)` after it
/// wakes. The returned handle keeps the subscription alive.
pub fn watch(on_change: impl Fn(bool) + 'static) -> Rc<RefCell<Option<gio::SignalSubscription>>> {
    let handle: Rc<RefCell<Option<gio::SignalSubscription>>> = Rc::new(RefCell::new(None));
    let h = handle.clone();
    gio::bus_get(gio::BusType::System, None::<&gio::Cancellable>, move |conn| {
        let conn = match conn {
            Ok(c) => c,
            Err(e) => {
                eprintln!("System bus unavailable, suspend detection disabled: {e}");
                return;
            }
        };
        let sub = conn.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some((start,)) = signal.parameters.get::<(bool,)>() {
                    on_change(start);
                }
            },
        );
        *h.borrow_mut() = Some(sub);
    });
    handle
}
