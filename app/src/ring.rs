//! Circular progress indicators drawn with cairo. Colours come from CSS (`color`), so they follow
//! the accent colour and the light/dark theme live.

use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::prelude::*;

/// A drawing area that strokes the arc between two fractions of a full turn.
fn arc_area(size: i32, line: f64, classes: &[&str], range: Rc<Cell<(f64, f64)>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(size)
        .content_height(size)
        .css_classes(classes.iter().copied())
        .can_target(false)
        .build();
    area.set_draw_func(move |area, cr, w, h| {
        let (from, to) = range.get();
        let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
        if to - from <= 0.0005 {
            return;
        }
        let c = area.color();
        cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), c.alpha().into());
        let r = (w.min(h) as f64 - line) / 2.0;
        cr.set_line_width(line);
        if to - from < 0.999 {
            cr.set_line_cap(gtk::cairo::LineCap::Round);
        }
        let start = -PI / 2.0 + from * 2.0 * PI;
        cr.arc(w as f64 / 2.0, h as f64 / 2.0, r, start, start + (to - from) * 2.0 * PI);
        let _ = cr.stroke();
    });
    area
}

/// The large ring around the countdown.
pub struct TimerRing {
    pub widget: gtk::Overlay,
    track: gtk::DrawingArea,
    progress: gtk::DrawingArea,
    value: Rc<Cell<(f64, f64)>>,
}

impl TimerRing {
    pub fn new(size: i32, center: &impl IsA<gtk::Widget>) -> TimerRing {
        let value = Rc::new(Cell::new((0.0, 0.0)));
        let track = arc_area(size, 8.0, &["ring-track"], Rc::new(Cell::new((0.0, 1.0))));
        let progress = arc_area(size, 8.0, &["ring-progress"], value.clone());
        let widget = gtk::Overlay::builder().child(&track).halign(gtk::Align::Center).build();
        widget.add_overlay(&progress);
        widget.add_overlay(center);
        TimerRing { widget, track, progress, value }
    }

    pub fn set(&self, fraction: f64, is_break: bool, paused: bool) {
        if self.value.get() != (0.0, fraction) {
            self.value.set((0.0, fraction));
            self.progress.queue_draw();
        }
        set_class(&self.progress, "break", is_break);
        set_class(&self.progress, "paused", paused);
    }

    pub fn areas(&self) -> [&gtk::DrawingArea; 2] {
        [&self.track, &self.progress]
    }
}

pub fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}

/// The daily goal ring: completed sessions solid, the running session as a lighter segment.
pub struct GoalRing {
    pub widget: gtk::Overlay,
    solid: gtk::DrawingArea,
    light: gtk::DrawingArea,
    solid_v: Rc<Cell<(f64, f64)>>,
    light_v: Rc<Cell<(f64, f64)>>,
}

impl GoalRing {
    pub fn new(size: i32) -> GoalRing {
        let solid_v = Rc::new(Cell::new((0.0, 0.0)));
        let light_v = Rc::new(Cell::new((0.0, 0.0)));
        let track = arc_area(size, 3.0, &["ring-track"], Rc::new(Cell::new((0.0, 1.0))));
        let solid = arc_area(size, 3.0, &["goal-ring"], solid_v.clone());
        let light = arc_area(size, 3.0, &["goal-ring"], light_v.clone());
        light.set_opacity(0.4);
        let widget = gtk::Overlay::builder().child(&track).valign(gtk::Align::Center).build();
        widget.add_overlay(&solid);
        widget.add_overlay(&light);
        GoalRing { widget, solid, light, solid_v, light_v }
    }

    /// `done` and `current` are in sessions; `goal` is the day's goal in sessions.
    pub fn set(&self, done: f64, current: f64, goal: f64) {
        let a = (done / goal.max(1.0)).min(1.0);
        let b = ((done + current) / goal.max(1.0)).min(1.0);
        self.solid_v.set((0.0, a));
        self.light_v.set((a, b));
        self.solid.queue_draw();
        self.light.queue_draw();
    }
}

/// One dot per pomodoro in the session: filled when completed, a pie for the running one.
pub struct SessionDots {
    pub widget: gtk::Box,
    dots: Vec<(gtk::DrawingArea, Rc<Cell<f64>>)>,
}

impl SessionDots {
    pub fn new() -> SessionDots {
        let widget = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        SessionDots { widget, dots: Vec::new() }
    }

    pub fn set(&mut self, target: u32, completed: u32, current: f64) {
        while self.dots.len() < target as usize {
            let fill = Rc::new(Cell::new(0.0));
            let area = gtk::DrawingArea::builder()
                .content_width(12)
                .content_height(12)
                .css_classes(["session-dot"])
                .build();
            let f = fill.clone();
            area.set_draw_func(move |area, cr, w, h| {
                let c = area.color();
                let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
                let r = w.min(h) as f64 / 2.0 - 1.0;
                cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), 0.35);
                cr.set_line_width(1.5);
                cr.arc(cx, cy, r, 0.0, 2.0 * PI);
                let _ = cr.stroke();
                let v = f.get();
                if v > 0.0 {
                    cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), c.alpha().into());
                    if v >= 1.0 {
                        cr.arc(cx, cy, r, 0.0, 2.0 * PI);
                    } else {
                        cr.move_to(cx, cy);
                        cr.arc(cx, cy, r, -PI / 2.0, -PI / 2.0 + v * 2.0 * PI);
                        cr.close_path();
                    }
                    let _ = cr.fill();
                }
            });
            self.widget.append(&area);
            self.dots.push((area, fill));
        }
        while self.dots.len() > target as usize {
            let (area, _) = self.dots.pop().unwrap();
            self.widget.remove(&area);
        }
        for (i, (area, fill)) in self.dots.iter().enumerate() {
            let i = i as u32;
            let v = if i < completed { 1.0 } else if i == completed { current } else { 0.0 };
            if fill.get() != v {
                fill.set(v);
                area.queue_draw();
            }
        }
        let label = format!("{completed} of {target} pomodoros completed");
        self.widget.update_property(&[gtk::accessible::Property::Label(&label)]);
    }
}
