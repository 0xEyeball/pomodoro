//! Statistics page: tag filter, heatmap, summary cards, streaks and per-tag breakdown.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use chrono::{Datelike, NaiveDate, TimeDelta, Weekday};
use gtk::glib;
use pomodoro_core::model::{Tag, TagColor};
use pomodoro_core::stats::{day_tooltip, month_bounds, ymd, Stats, TagFilter, Totals};
use pomodoro_core::time::{fmt_decimal, fmt_focus};

use crate::ctx::{label_widget, tag_class, tag_dot, Change, Ctx};
use crate::ring::set_class;
use crate::week::first_weekday;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Period {
    Day,
    Month,
    Year,
    All,
}

const PERIODS: [(Period, &str); 4] =
    [(Period::Day, "Day"), (Period::Month, "Month"), (Period::Year, "Year"), (Period::All, "All time")];

/// One summary card.
struct Card {
    widget: gtk::Box,
    title: gtk::Label,
    pomodoros: [gtk::Label; 3],
    sessions: [gtk::Label; 3],
    focus: gtk::Label,
    prev: Option<gtk::Button>,
    next: Option<gtk::Button>,
}

impl Card {
    fn new(nav: bool) -> Card {
        let title = gtk::Label::builder().css_classes(["heading"]).hexpand(true).build();
        let header = gtk::Box::builder().spacing(6).build();
        let mk = |icon: &str, label: &str| {
            let b = gtk::Button::builder().icon_name(icon).css_classes(["flat", "circular"]).build();
            label_widget(&b, label);
            b
        };
        let (prev, next) = if nav {
            (Some(mk("go-previous-symbolic", "Previous")), Some(mk("go-next-symbolic", "Next")))
        } else {
            (None, None)
        };
        if let Some(p) = &prev {
            header.append(p);
        }
        header.append(&title);
        if let Some(n) = &next {
            header.append(n);
        }
        // A small table: rows Pomodoros / Sessions / Focus, columns complete / incomplete / total.
        let value = || gtk::Label::builder().xalign(1.0).css_classes(["stat-value"]).build();
        let pomodoros = [value(), value(), value()];
        let sessions = [value(), value(), value()];
        let focus = gtk::Label::builder().xalign(1.0).css_classes(["stat-value"]).build();
        let grid = gtk::Grid::builder().row_spacing(4).column_spacing(12).halign(gtk::Align::Center).build();
        let dim = |text: &str, x: f32, classes: &[&str]| {
            gtk::Label::builder().label(text).xalign(x).css_classes(classes.iter().map(|c| c.to_string()).collect::<Vec<_>>()).build()
        };
        for (col, head) in ["Complete", "Incomplete", "Total"].into_iter().enumerate() {
            grid.attach(&dim(head, 1.0, &["dim-label", "caption"]), col as i32 + 1, 0, 1, 1);
        }
        for (row, (name, cells)) in [("Pomodoros", &pomodoros), ("Sessions", &sessions)].into_iter().enumerate() {
            grid.attach(&dim(name, 0.0, &["dim-label"]), 0, row as i32 + 1, 1, 1);
            for (col, cell) in cells.iter().enumerate() {
                grid.attach(cell, col as i32 + 1, row as i32 + 1, 1, 1);
            }
        }
        grid.attach(&dim("Focus", 0.0, &["dim-label"]), 0, 3, 1, 1);
        grid.attach(&focus, 1, 3, 3, 1);
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .css_classes(["card", "stat-card"])
            .build();
        widget.append(&header);
        widget.append(&grid);
        Card { widget, title, pomodoros, sessions, focus, prev, next }
    }

    fn set(&self, title: &str, t: &Totals) {
        self.title.set_label(title);
        let fill = |cells: &[gtk::Label; 3], c: u32, i: u32, total: f64, what: &str| {
            cells[0].set_label(&c.to_string());
            cells[1].set_label(&i.to_string());
            cells[2].set_label(&fmt_decimal(total));
            // Full precision for the curious.
            cells[2].set_tooltip_text(Some(&format!("{total} {what}")));
        };
        fill(&self.pomodoros, t.pom_completed, t.pom_incomplete, t.pom_total, "pomodoros");
        fill(&self.sessions, t.ses_completed, t.ses_incomplete, t.ses_total, "sessions");
        self.focus.set_label(&fmt_focus(t.focus_sec));
        self.focus.set_tooltip_text(Some(&format!("{:.0} seconds", t.focus_sec)));
    }
}

pub struct StatsView {
    pub widget: gtk::ScrolledWindow,
    me: Weak<StatsView>,
    ctx: Rc<Ctx>,
    filter_drop: gtk::DropDown,
    filters: RefCell<Vec<TagFilter>>,
    year_drop: gtk::DropDown,
    years: RefCell<Vec<Option<i32>>>,
    heat_scroll: gtk::ScrolledWindow,
    heat_box: gtk::Box,
    heat_days: gtk::Box,
    legend: gtk::Box,
    cells: RefCell<HashMap<NaiveDate, gtk::Button>>,
    selected: Cell<NaiveDate>,
    /// The "today" the view last rendered for, to follow the date across midnight.
    today: Cell<NaiveDate>,
    month: Cell<(i32, u32)>,
    year: Cell<i32>,
    day_card: Card,
    month_card: Card,
    year_card: Card,
    all_card: Card,
    streak_current: gtk::Label,
    streak_longest: gtk::Label,
    best_day: gtk::Label,
    tag_group: adw::PreferencesGroup,
    tag_rows: RefCell<Vec<adw::ActionRow>>,
    tag_period: gtk::DropDown,
    stats: RefCell<Stats>,
    tags: RefCell<HashMap<String, Tag>>,
    dirty: Cell<bool>,
    updating: Cell<bool>,
}

impl StatsView {
    pub fn new(ctx: &Rc<Ctx>) -> Rc<StatsView> {
        let today = ctx.engine.borrow().today_date();

        let filter_drop = gtk::DropDown::from_strings(&[]);
        label_widget(&filter_drop, "Tag filter");
        let year_drop = gtk::DropDown::from_strings(&[]);
        label_widget(&year_drop, "Heatmap range");
        let controls = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
        controls.append(&filter_drop);
        controls.append(&year_drop);

        let heat_box = gtk::Box::builder().css_classes(["heatmap"]).build();
        let heat_scroll = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&heat_box)
            .build();
        // Weekday labels stay put while the weeks scroll.
        let heat_days = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let heat_row = gtk::Box::builder().spacing(4).build();
        heat_row.append(&heat_days);
        heat_row.append(&heat_scroll);
        let legend = gtk::Box::builder().spacing(3).halign(gtk::Align::End).css_classes(["heatmap"]).build();
        legend.append(&gtk::Label::builder().label("Less").css_classes(["heat-axis"]).margin_end(3).build());
        for level in 0..=4 {
            let cell = gtk::Box::builder().css_classes(["heat-cell", &format!("heat-{level}")]).valign(gtk::Align::Center).build();
            legend.append(&cell);
        }
        legend.append(&gtk::Label::builder().label("More").css_classes(["heat-axis"]).margin_start(3).build());
        legend.update_property(&[gtk::accessible::Property::Label(
            "Colour intensity grows with the number of pomodoros, up to the daily goal",
        )]);

        let cards = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(2)
            .min_children_per_line(1)
            .column_spacing(12)
            .row_spacing(12)
            .homogeneous(true)
            .build();
        let day_card = Card::new(false);
        let month_card = Card::new(true);
        let year_card = Card::new(true);
        let all_card = Card::new(false);
        for c in [&day_card, &month_card, &year_card, &all_card] {
            cards.append(&c.widget);
        }

        let streaks = adw::PreferencesGroup::builder().title("Streaks").build();
        let suffix = || gtk::Label::builder().css_classes(["stat-value"]).build();
        let (streak_current, streak_longest, best_day) = (suffix(), suffix(), suffix());
        for (title, l) in [("Current streak", &streak_current), ("Longest streak", &streak_longest), ("Best day", &best_day)] {
            let row = adw::ActionRow::builder().title(title).build();
            row.add_suffix(l);
            streaks.add(&row);
        }

        let tag_period = gtk::DropDown::from_strings(&PERIODS.map(|(_, n)| n));
        tag_period.set_selected(1);
        tag_period.set_valign(gtk::Align::Center);
        label_widget(&tag_period, "Breakdown period");
        let tag_group = adw::PreferencesGroup::builder().title("By Tag").header_suffix(&tag_period).build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&controls);
        content.append(&heat_row);
        content.append(&legend);
        content.append(&cards);
        content.append(&streaks);
        content.append(&tag_group);
        let clamp = adw::Clamp::builder().maximum_size(860).child(&content).build();
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();

        let view = Rc::new_cyclic(|me| StatsView {
            widget,
            me: me.clone(),
            ctx: ctx.clone(),
            filter_drop,
            filters: RefCell::new(vec![TagFilter::All]),
            year_drop,
            years: RefCell::new(vec![None]),
            heat_scroll,
            heat_box,
            heat_days,
            legend,
            cells: RefCell::default(),
            selected: Cell::new(today),
            today: Cell::new(today),
            month: Cell::new((today.year(), today.month())),
            year: Cell::new(today.year()),
            day_card,
            month_card,
            year_card,
            all_card,
            streak_current,
            streak_longest,
            best_day,
            tag_group,
            tag_rows: RefCell::default(),
            tag_period,
            stats: RefCell::default(),
            tags: RefCell::default(),
            dirty: Cell::new(true),
            updating: Cell::new(false),
        });

        let redraw = |v: &Rc<StatsView>| {
            if !v.updating.get() {
                v.render();
            }
        };
        view.filter_drop.connect_selected_notify(glib::clone!(#[weak] view, move |_| redraw(&view)));
        view.year_drop.connect_selected_notify(glib::clone!(#[weak] view, move |_| redraw(&view)));
        view.tag_period.connect_selected_notify(glib::clone!(#[weak] view, move |_| view.render_figures()));

        let step_month = |v: &StatsView, delta: i32| {
            let (y, m) = v.month.get();
            let idx = y * 12 + m as i32 - 1 + delta;
            v.month.set((idx.div_euclid(12), idx.rem_euclid(12) as u32 + 1));
            v.render_figures();
        };
        let step_year = |v: &StatsView, delta: i32| {
            v.year.set(v.year.get() + delta);
            v.render_figures();
        };
        if let (Some(p), Some(n)) = (&view.month_card.prev, &view.month_card.next) {
            p.connect_clicked(glib::clone!(#[weak] view, move |_| step_month(&view, -1)));
            n.connect_clicked(glib::clone!(#[weak] view, move |_| step_month(&view, 1)));
        }
        if let (Some(p), Some(n)) = (&view.year_card.prev, &view.year_card.next) {
            p.connect_clicked(glib::clone!(#[weak] view, move |_| step_year(&view, -1)));
            n.connect_clicked(glib::clone!(#[weak] view, move |_| step_year(&view, 1)));
        }

        let weak = Rc::downgrade(&view);
        ctx.subscribe(move |change| {
            if let Some(v) = weak.upgrade() {
                if matches!(change, Change::Data | Change::Tags | Change::Settings) {
                    v.dirty.set(true);
                    if v.widget.is_mapped() {
                        v.refresh();
                    }
                }
            }
        });
        view.widget.connect_map(glib::clone!(
            #[weak]
            view,
            move |_| {
                if view.dirty.get() {
                    view.refresh();
                }
            }
        ));
        view
    }

    fn filter(&self) -> TagFilter {
        self.filters.borrow().get(self.filter_drop.selected() as usize).cloned().unwrap_or(TagFilter::All)
    }

    fn period(&self) -> Period {
        PERIODS.get(self.tag_period.selected() as usize).map(|p| p.0).unwrap_or(Period::Month)
    }

    /// Reloads records from the database and rebuilds every figure.
    pub fn refresh(&self) {
        self.dirty.set(false);
        let today = self.ctx.engine.borrow().today_date();
        let before = self.today.replace(today);
        if before != today {
            // Periods still showing the old "today" move along with it.
            if self.selected.get() == before {
                self.selected.set(today);
            }
            if self.month.get() == (before.year(), before.month()) {
                self.month.set((today.year(), today.month()));
            }
            if self.year.get() == before.year() {
                self.year.set(today.year());
            }
        }
        let stats = Stats::load(self.ctx.engine.borrow().db());
        *self.stats.borrow_mut() = self.ctx.report(stats).unwrap_or_default();
        let tags = self.ctx.all_tags();
        *self.tags.borrow_mut() = tags.iter().map(|t| (t.id.clone(), t.clone())).collect();

        self.updating.set(true);
        // Tag filter: All, Untagged, active tags, then archived tags.
        let current = self.filter();
        let mut filters = vec![TagFilter::All, TagFilter::Untagged];
        let mut names = vec!["All tags".to_owned(), "Untagged".to_owned()];
        let mut sorted = tags.clone();
        sorted.sort_by_key(|t| t.archived);
        for t in &sorted {
            filters.push(TagFilter::Tag(t.id.clone()));
            names.push(if t.archived { format!("{} (archived)", t.name) } else { t.name.clone() });
        }
        set_strings(&self.filter_drop, &names);
        self.filter_drop.set_selected(filters.iter().position(|f| *f == current).unwrap_or(0) as u32);
        *self.filters.borrow_mut() = filters;

        // Range: last 12 months, then every year with data.
        let current_year = self.years.borrow().get(self.year_drop.selected() as usize).copied().flatten();
        let mut years = vec![None];
        years.extend(self.stats.borrow().years().into_iter().map(Some));
        let labels: Vec<String> = years
            .iter()
            .map(|y| y.map_or_else(|| "Last 12 months".to_owned(), |y| y.to_string()))
            .collect();
        set_strings(&self.year_drop, &labels);
        self.year_drop
            .set_selected(years.iter().position(|y| *y == current_year).unwrap_or(0) as u32);
        *self.years.borrow_mut() = years;
        self.updating.set(false);

        self.render();
    }

    fn render(&self) {
        self.render_heatmap();
        self.render_figures();
    }

    fn render_heatmap(&self) {
        let filter = self.filter();
        let today = self.ctx.engine.borrow().today_date();
        let year = self.years.borrow().get(self.year_drop.selected() as usize).copied().flatten();
        let (start, end) = match year {
            None => (today - TimeDelta::days(364), today),
            Some(y) => (ymd(y, 1, 1), ymd(y, 12, 31)),
        };
        let week_start = first_weekday();
        let offset = |d: NaiveDate| (d.weekday().num_days_from_monday() + 7 - week_start.num_days_from_monday()) % 7;
        let grid_start = start - TimeDelta::days(offset(start) as i64);

        let color = match &filter {
            TagFilter::Tag(id) => self.tags.borrow().get(id).map(|t| t.color),
            _ => None,
        };
        for target in [&self.heat_box, &self.legend] {
            for &c in TagColor::ALL {
                target.remove_css_class(&tag_class(c));
            }
            if let Some(c) = color {
                target.add_css_class(&tag_class(c));
            }
        }

        let grid = gtk::Grid::builder().row_spacing(3).column_spacing(3).build();
        let stats = self.stats.borrow();
        let mut cells = HashMap::new();
        // Month labels: (column, first day of that month) wherever a month begins.
        let mut month_starts: Vec<(i32, NaiveDate)> = Vec::new();
        let mut d = grid_start;
        while d <= end {
            let idx = (d - grid_start).num_days() as i32;
            let (col, row) = (idx / 7, idx % 7 + 1);
            if d >= start {
                if d == start || d.day() == 1 {
                    month_starts.push((col, d));
                }
                let t = stats.day(d, &filter);
                let level = stats.heat_level_on(d, &filter);
                let tip = day_tooltip(d, &t);
                let cell = gtk::Button::builder()
                    .css_classes(["heat-cell", &format!("heat-{level}")])
                    .tooltip_text(&tip)
                    .build();
                cell.update_property(&[gtk::accessible::Property::Label(&tip)]);
                let me = self.me.clone();
                cell.connect_clicked(move |_| {
                    if let Some(v) = me.upgrade() {
                        v.select_day(d);
                    }
                });
                grid.attach(&cell, col, row, 1, 1);
                cells.insert(d, cell);
            }
            d += TimeDelta::days(1);
        }
        drop(stats);
        // Skip a partial first month when the next label would collide with it.
        let mut last_col: Option<i32> = None;
        for (i, &(col, date)) in month_starts.iter().enumerate() {
            let crowded_next = month_starts.get(i + 1).is_some_and(|&(next, _)| next - col < 3);
            if crowded_next || last_col.is_some_and(|c| col - c < 3) {
                continue;
            }
            let l = gtk::Label::builder()
                .label(date.format("%b").to_string())
                .xalign(0.0)
                .css_classes(["heat-axis"])
                .build();
            grid.attach(&l, col, 0, 3, 1);
            last_col = Some(col);
        }
        // Weekday labels in a separate grid with the same row geometry (row 0 = month row).
        let days = gtk::Grid::builder().row_spacing(3).row_homogeneous(false).build();
        days.attach(&gtk::Label::builder().label(" ").css_classes(["heat-axis"]).build(), 0, 0, 1, 1);
        for row in 1..=7 {
            let weekday = Weekday::try_from(((week_start.num_days_from_monday() + row as u32 - 1) % 7) as u8).unwrap_or(Weekday::Mon);
            let shown = matches!(weekday, Weekday::Mon | Weekday::Wed | Weekday::Fri);
            let l = gtk::Label::builder()
                .label(if shown { weekday_abbrev(weekday) } else { String::new() })
                .xalign(0.0)
                .css_classes(["heat-axis", "heat-day"])
                .build();
            days.attach(&l, 0, row, 1, 1);
        }
        if let Some(old) = self.heat_days.first_child() {
            self.heat_days.remove(&old);
        }
        self.heat_days.append(&days);

        while let Some(child) = self.heat_box.first_child() {
            self.heat_box.remove(&child);
        }
        self.heat_box.append(&grid);
        *self.cells.borrow_mut() = cells;
        self.mark_selected();

        // Show the most recent weeks first.
        let adj = self.heat_scroll.hadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
    }

    fn mark_selected(&self) {
        for (d, c) in self.cells.borrow().iter() {
            set_class(c, "selected", *d == self.selected.get());
        }
    }

    fn select_day(&self, d: NaiveDate) {
        self.selected.set(d);
        self.mark_selected();
        self.render_figures();
    }

    fn render_figures(&self) {
        let filter = self.filter();
        let stats = self.stats.borrow();
        let today = self.ctx.engine.borrow().today_date();

        let day = self.selected.get();
        let title = if day == today { "Today".to_owned() } else { day.format("%a %-d %b %Y").to_string() };
        self.day_card.set(&title, &stats.day(day, &filter));
        let (y, m) = self.month.get();
        self.month_card.set(&ymd(y, m, 1).format("%B %Y").to_string(), &stats.month(y, m, &filter));
        let year = self.year.get();
        self.year_card.set(&year.to_string(), &stats.year(year, &filter));
        self.all_card.set("All Time", &stats.all_time(&filter));

        let (current, longest) = stats.streaks(today, &filter);
        let days = |n: u32| format!("{n} day{}", if n == 1 { "" } else { "s" });
        self.streak_current.set_label(&days(current));
        self.streak_longest.set_label(&days(longest));
        self.best_day.set_label(&match stats.best_day(&filter) {
            Some((d, t)) => format!("{} · {} pomodoros", d.format("%-d %b %Y"), fmt_decimal(t.pom_total)),
            None => "—".to_owned(),
        });

        let (from, to) = match self.period() {
            Period::Day => (day, day),
            Period::Month => month_bounds(y, m),
            Period::Year => (ymd(year, 1, 1), ymd(year, 12, 31)),
            Period::All => (NaiveDate::MIN, NaiveDate::MAX),
        };
        let rows = stats.by_tag(from, to);
        drop(stats);
        for r in self.tag_rows.borrow_mut().drain(..) {
            self.tag_group.remove(&r);
        }
        let tags = self.tags.borrow();
        let mut new_rows = Vec::new();
        for (tag, t) in rows {
            let matches = match (&filter, &tag) {
                (TagFilter::All, _) => true,
                (TagFilter::Untagged, None) => true,
                (TagFilter::Tag(f), Some(id)) => f == id,
                _ => false,
            };
            if !matches {
                continue;
            }
            let info = tag.as_ref().and_then(|id| tags.get(id));
            let row = adw::ActionRow::builder()
                .title(info.map(|t| t.name.as_str()).unwrap_or("Untagged"))
                .subtitle(format!(
                    "{} pomodoros · {} sessions complete · {} focus",
                    fmt_decimal(t.pom_total),
                    t.ses_completed,
                    fmt_focus(t.focus_sec)
                ))
                .build();
            row.add_prefix(&tag_dot(info.map(|t| t.color)));
            self.tag_group.add(&row);
            new_rows.push(row);
        }
        if new_rows.is_empty() {
            let row = adw::ActionRow::builder().title("Nothing recorded in this period").build();
            row.add_css_class("dim-label");
            self.tag_group.add(&row);
            new_rows.push(row);
        }
        *self.tag_rows.borrow_mut() = new_rows;
    }
}

fn set_strings(drop: &gtk::DropDown, items: &[String]) {
    let model = gtk::StringList::new(&items.iter().map(String::as_str).collect::<Vec<_>>());
    drop.set_model(Some(&model));
}

fn weekday_abbrev(d: Weekday) -> String {
    // Any date with the right weekday, formatted with the locale's abbreviation.
    let base = ymd(2024, 1, 1); // a Monday
    (base + TimeDelta::days(d.num_days_from_monday() as i64)).format("%a").to_string()
}
