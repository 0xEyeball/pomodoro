//! The main timer screen.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use pomodoro_core::engine::Snapshot;
use pomodoro_core::model::{Mode, PhaseKind, RunState, Tag};
use pomodoro_core::time::{fmt_clock, parse_ts, started_ago};

use crate::ctx::{set_tag_dot_color, tag_dot, Change, Ctx};
use crate::dialogs;
use crate::ring::{set_class, GoalRing, SessionDots, TimerRing};

const NO_TAG: &str = "";
const NEW_TAG: &str = "\u{1}new";

pub struct TimerView {
    pub widget: gtk::ScrolledWindow,
    ctx: Rc<Ctx>,
    ring: TimerRing,
    phase_label: gtk::Label,
    digits: gtk::Label,
    state_label: gtk::Label,
    hint: gtk::Label,
    dots: RefCell<SessionDots>,
    tag_model: gtk::StringList,
    tag_drop: gtk::DropDown,
    tag_lookup: Rc<RefCell<HashMap<String, Tag>>>,
    blitz: gtk::Switch,
    primary: gtk::Button,
    cancel_pomodoro: gtk::Button,
    cancel_session: gtk::Button,
    skip_break: gtk::Button,
    goal_ring: GoalRing,
    goal_stack: gtk::Stack,
    footer: gtk::Label,
    updating: Cell<bool>,
}

impl TimerView {
    pub fn new(ctx: &Rc<Ctx>) -> Rc<TimerView> {
        let phase_label = gtk::Label::builder().css_classes(["phase-label"]).build();
        let digits = gtk::Label::builder().css_classes(["timer-digits"]).build();
        let state_label = gtk::Label::builder().css_classes(["dim-label", "caption"]).build();
        let center = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        center.append(&phase_label);
        center.append(&digits);
        center.append(&state_label);
        let ring = TimerRing::new(220, &center);

        let hint = gtk::Label::builder().css_classes(["dim-label", "caption"]).visible(false).build();

        let tag_model = gtk::StringList::new(&[]);
        let tag_lookup: Rc<RefCell<HashMap<String, Tag>>> = Rc::default();
        let tag_drop = gtk::DropDown::builder().model(&tag_model).build();
        tag_drop.set_factory(Some(&tag_factory(tag_lookup.clone())));
        tag_drop.update_property(&[gtk::accessible::Property::Label("Tag")]);
        tag_drop.set_tooltip_text(Some("Tag"));

        let blitz = gtk::Switch::builder().valign(gtk::Align::Center).build();
        blitz.update_property(&[gtk::accessible::Property::Label("Blitz mode")]);
        let blitz_label = gtk::Label::new(Some("Blitz"));
        blitz_label.set_mnemonic_widget(Some(&blitz));
        let blitz_box = gtk::Box::builder().spacing(6).tooltip_text("Blitz: skip short breaks").build();
        blitz_box.append(&blitz_label);
        blitz_box.append(&blitz);

        let options = gtk::Box::builder().spacing(18).halign(gtk::Align::Center).build();
        options.append(&tag_drop);
        options.append(&blitz_box);

        let primary = gtk::Button::builder()
            .action_name("win.toggle")
            .halign(gtk::Align::Center)
            .width_request(160)
            .css_classes(["pill", "suggested-action"])
            .build();

        let small = |label: &str, action: &str| {
            gtk::Button::builder().label(label).action_name(action).css_classes(["flat"]).build()
        };
        let cancel_pomodoro = small("Cancel Pomodoro", "win.cancel-pomodoro");
        let cancel_session = small("Cancel Session", "win.cancel-session");
        let skip_break = small("Skip Break", "win.skip-break");
        let secondary = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).build();
        secondary.append(&skip_break);
        secondary.append(&cancel_pomodoro);
        secondary.append(&cancel_session);

        let goal_ring = GoalRing::new(18);
        let check = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .css_classes(["goal-done"])
            .build();
        let goal_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        goal_stack.add_named(&goal_ring.widget, Some("ring"));
        goal_stack.add_named(&check, Some("done"));
        let footer = gtk::Label::builder().css_classes(["dim-label"]).wrap(true).build();
        let footer_box = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).margin_top(6).build();
        footer_box.append(&goal_stack);
        footer_box.append(&footer);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .valign(gtk::Align::Center)
            .build();
        content.append(&hint);
        content.append(&ring.widget);
        let dots = SessionDots::new();
        content.append(&dots.widget);
        content.append(&options);
        content.append(&primary);
        content.append(&secondary);
        content.append(&footer_box);

        let clamp = adw::Clamp::builder().maximum_size(420).child(&content).build();
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();

        let view = Rc::new(TimerView {
            widget,
            ctx: ctx.clone(),
            ring,
            phase_label,
            digits,
            state_label,
            hint,
            dots: RefCell::new(dots),
            tag_model,
            tag_drop,
            tag_lookup,
            blitz,
            primary,
            cancel_pomodoro,
            cancel_session,
            skip_break,
            goal_ring,
            goal_stack,
            footer,
            updating: Cell::new(false),
        });

        view.tag_drop.connect_selected_notify(glib::clone!(
            #[weak]
            view,
            move |_| view.on_tag_selected()
        ));
        view.blitz.connect_active_notify(glib::clone!(
            #[weak]
            view,
            move |sw| {
                if !view.updating.get() && view.ctx.engine.borrow().snapshot().session.is_none() {
                    *view.ctx.next_mode.borrow_mut() = if sw.is_active() { Mode::Blitz } else { Mode::Standard };
                }
            }
        ));

        let weak = Rc::downgrade(&view);
        ctx.subscribe(move |change| {
            if let Some(v) = weak.upgrade() {
                if matches!(change, Change::Tags | Change::Data) {
                    v.rebuild_tags();
                }
                v.update();
            }
        });
        view.rebuild_tags();
        view.update();
        view
    }

    /// Breakpoint hook: shrink the ring on narrow windows.
    pub fn ring_areas(&self) -> [&gtk::DrawingArea; 2] {
        self.ring.areas()
    }

    fn selected_id(&self) -> Option<String> {
        let item = self.tag_model.string(self.tag_drop.selected())?;
        Some(item.to_string())
    }

    fn current_tag(&self) -> Option<String> {
        match self.ctx.engine.borrow().snapshot().session {
            Some(s) => s.tag_id,
            None => self.ctx.next_tag.borrow().clone(),
        }
    }

    fn rebuild_tags(&self) {
        self.updating.set(true);
        let current = self.current_tag();
        let mut tags = self.ctx.active_tags();
        // Keep an archived tag visible while the active session still uses it.
        if let Some(id) = &current {
            if !tags.iter().any(|t| &t.id == id) {
                if let Some(t) = self.ctx.all_tags().into_iter().find(|t| &t.id == id) {
                    tags.push(t);
                }
            }
        }
        let mut ids: Vec<&str> = vec![NO_TAG];
        ids.extend(tags.iter().map(|t| t.id.as_str()));
        ids.push(NEW_TAG);
        self.tag_model.splice(0, self.tag_model.n_items(), &ids);
        *self.tag_lookup.borrow_mut() = tags.iter().map(|t| (t.id.clone(), t.clone())).collect();
        self.select_tag(current.as_deref());
        self.updating.set(false);
    }

    fn select_tag(&self, id: Option<&str>) {
        let want = id.unwrap_or(NO_TAG);
        let pos = (0..self.tag_model.n_items())
            .find(|&i| self.tag_model.string(i).is_some_and(|s| s == want))
            .unwrap_or(0);
        if self.tag_drop.selected() != pos {
            let was = self.updating.replace(true);
            self.tag_drop.set_selected(pos);
            self.updating.set(was);
        }
    }

    fn on_tag_selected(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        let Some(id) = self.selected_id() else { return };
        if id == NEW_TAG {
            // Put the selection back until a tag is actually created.
            self.select_tag(self.current_tag().as_deref());
            let view = self.clone();
            dialogs::new_tag(&self.ctx, move |tag| {
                view.ctx.notify(Change::Tags);
                view.apply_tag(Some(tag.id));
            });
            return;
        }
        self.apply_tag((!id.is_empty()).then_some(id));
    }

    fn apply_tag(&self, tag: Option<String>) {
        let in_session = self.ctx.engine.borrow().snapshot().session.is_some();
        if in_session {
            let r = self.ctx.engine.borrow_mut().set_session_tag(tag.clone());
            self.ctx.report(r);
        } else {
            *self.ctx.next_tag.borrow_mut() = tag.clone();
        }
        if self.ctx.settings().remember_last_tag {
            self.ctx.update_settings(|s| s.last_tag_id = tag.clone());
        }
        self.select_tag(tag.as_deref());
        if in_session {
            self.ctx.notify(Change::Data);
        }
    }

    pub fn update(&self) {
        let (snap, today, settings, now) = {
            let e = self.ctx.engine.borrow();
            (e.snapshot(), e.today(), e.settings().clone(), e.now())
        };
        let today = self.ctx.report(today);
        self.updating.set(true);

        let (phase_text, state_text) = phase_texts(&snap);
        self.phase_label.set_label(&phase_text);
        self.state_label.set_label(&state_text);
        self.state_label.set_visible(!state_text.is_empty());
        let remaining = match snap.phase {
            None => settings.focus_min as f64 * 60.0,
            Some(_) => snap.remaining_sec(),
        };
        self.digits.set_label(&fmt_clock(remaining));
        self.digits.update_property(&[gtk::accessible::Property::Label(&format!(
            "{phase_text}, {} remaining",
            fmt_clock(remaining)
        ))]);

        let is_break = snap.phase.is_some_and(PhaseKind::is_break);
        self.ring.set(snap.progress(), is_break, snap.run_state == RunState::Paused);

        {
            let mut dots = self.dots.borrow_mut();
            match &snap.session {
                Some(s) => dots.set(s.target, s.completed, s.current_fraction),
                None if snap.phase == Some(PhaseKind::LongBreak) => {
                    dots.set(settings.pomodoros_per_session, settings.pomodoros_per_session, 0.0)
                }
                None => dots.set(settings.pomodoros_per_session, 0, 0.0),
            }
        }

        let hint = snap
            .session
            .as_ref()
            .and_then(|s| parse_ts(&s.started_at))
            .and_then(|t| started_ago(t, now));
        self.hint.set_visible(hint.is_some());
        self.hint.set_label(hint.as_deref().unwrap_or(""));

        match &snap.session {
            Some(s) => {
                self.select_tag(s.tag_id.as_deref());
                self.blitz.set_active(s.mode == Mode::Blitz);
                self.blitz.set_sensitive(false);
            }
            None => {
                self.select_tag(self.ctx.next_tag.borrow().as_deref());
                self.blitz.set_active(*self.ctx.next_mode.borrow() == Mode::Blitz);
                self.blitz.set_sensitive(true);
            }
        }

        let primary = match snap.run_state {
            RunState::Running => "Pause",
            RunState::Paused => "Resume",
            RunState::Waiting => "Start",
        };
        self.primary.set_label(primary);
        set_class(&self.primary, "suggested-action", snap.run_state != RunState::Running);

        let can_skip = self.ctx.engine.borrow().can_skip_break();
        self.cancel_pomodoro.set_visible(snap.in_focus());
        self.cancel_session.set_visible(snap.session.is_some());
        self.skip_break.set_visible(can_skip);

        if let Some(t) = today {
            let current = snap.session.as_ref().map(|s| s.fraction()).unwrap_or(0.0);
            self.goal_ring.set(t.completed_sessions as f64, current, t.goal_sessions as f64);
            self.footer.set_label(&format!(
                "Today: {} / {} sessions · {} pomodoro{}",
                t.completed_sessions,
                t.goal_sessions,
                t.completed_pomodoros,
                if t.completed_pomodoros == 1 { "" } else { "s" }
            ));
        }
        self.updating.set(false);
    }

    /// The brief, one-time acknowledgement when the daily goal is reached.
    pub fn flash_goal_reached(&self) {
        self.goal_stack.set_visible_child_name("done");
        let stack = self.goal_stack.downgrade();
        glib::timeout_add_seconds_local_once(4, move || {
            if let Some(s) = stack.upgrade() {
                s.set_visible_child_name("ring");
            }
        });
    }

    pub fn current_selection(&self) -> (Option<String>, Mode) {
        (self.ctx.next_tag.borrow().clone(), *self.ctx.next_mode.borrow())
    }
}

/// (phase label, state line) for the timer.
fn phase_texts(snap: &Snapshot) -> (String, String) {
    match (snap.phase, snap.run_state) {
        (None, _) => ("Ready".into(), "New session".into()),
        (Some(k), RunState::Waiting) => (k.label().into(), "Up next".into()),
        (Some(k), RunState::Paused) => (k.label().into(), "Paused".into()),
        (Some(k), RunState::Running) => (k.label().into(), String::new()),
    }
}

/// Window title: `18:42 · Focus`.
pub fn window_title(snap: &Snapshot) -> String {
    match snap.phase {
        Some(k) if snap.in_phase() => {
            let paused = if snap.run_state == RunState::Paused { " (paused)" } else { "" };
            format!("{} · {}{paused}", fmt_clock(snap.remaining_sec()), k.label())
        }
        _ => "Pomodoro".into(),
    }
}

fn tag_factory(lookup: Rc<RefCell<HashMap<String, Tag>>>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let row = gtk::Box::builder().spacing(8).build();
        row.append(&tag_dot(None));
        row.append(&gtk::Label::builder().xalign(0.0).build());
        item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let id = item
            .item()
            .and_downcast::<gtk::StringObject>()
            .map(|s| s.string().to_string())
            .unwrap_or_default();
        let row = item.child().and_downcast::<gtk::Box>().expect("row");
        let dot = row.first_child().and_downcast::<gtk::Box>().expect("dot");
        let label = row.last_child().and_downcast::<gtk::Label>().expect("label");
        let lookup = lookup.borrow();
        let (text, color, show_dot) = match id.as_str() {
            NO_TAG => ("No tag".to_owned(), None, true),
            NEW_TAG => ("New tag…".to_owned(), None, false),
            _ => match lookup.get(&id) {
                Some(t) => (t.name.clone(), Some(t.color), true),
                None => ("Unknown tag".to_owned(), None, true),
            },
        };
        label.set_label(&text);
        dot.set_visible(show_dot);
        set_tag_dot_color(&dot, color);
    });
    factory
}
