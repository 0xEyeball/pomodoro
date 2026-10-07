//! Statistics derived from stored records. Nothing here is cached between loads: every figure
//! is recomputed from `sessions` and `pomodoros`.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, NaiveDate, TimeDelta};

use crate::db::{goal_for, Db, Result};
use crate::model::*;
use crate::time::{fmt_decimal, fmt_focus};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TagFilter {
    All,
    Tag(String),
    Untagged,
}

impl TagFilter {
    fn matches(&self, tag: &Option<String>) -> bool {
        match self {
            TagFilter::All => true,
            TagFilter::Tag(id) => tag.as_deref() == Some(id),
            TagFilter::Untagged => tag.is_none(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub pom_completed: u32,
    pub pom_incomplete: u32,
    pub pom_total: f64,
    pub ses_completed: u32,
    pub ses_incomplete: u32,
    pub ses_total: f64,
    pub focus_sec: f64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.pom_completed += o.pom_completed;
        self.pom_incomplete += o.pom_incomplete;
        self.pom_total += o.pom_total;
        self.ses_completed += o.ses_completed;
        self.ses_incomplete += o.ses_incomplete;
        self.ses_total += o.ses_total;
        self.focus_sec += o.focus_sec;
    }

    pub fn is_empty(&self) -> bool {
        self.pom_total == 0.0 && self.ses_total == 0.0 && self.focus_sec == 0.0
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Totals per day, split by tag (None = untagged). Ordered maps keep float sums deterministic.
    days: BTreeMap<NaiveDate, BTreeMap<Option<String>, Totals>>,
    goals: Vec<GoalEntry>,
}

impl Stats {
    pub fn load(db: &Db) -> Result<Stats> {
        Ok(Stats::build(&db.sessions()?, &db.pomodoros()?, db.goal_history()?))
    }

    pub fn build(sessions: &[Session], pomodoros: &[Pomodoro], goals: Vec<GoalEntry>) -> Stats {
        let mut stats = Stats { days: BTreeMap::new(), goals };
        let tag_of: HashMap<&str, &Option<String>> =
            sessions.iter().map(|s| (s.id.as_str(), &s.tag_id)).collect();
        let mut equivalents: HashMap<&str, f64> = HashMap::new();

        for p in pomodoros {
            let tag = tag_of.get(p.session_id.as_str()).map(|t| (*t).clone()).unwrap_or(None);
            let t = stats.entry(p.local_date, tag);
            match p.status {
                PomodoroStatus::Completed => {
                    t.pom_completed += 1;
                    t.pom_total += 1.0;
                }
                PomodoroStatus::Cancelled if p.elapsed_sec > 0.0 => {
                    t.pom_incomplete += 1;
                    t.pom_total += p.fraction();
                }
                PomodoroStatus::Cancelled => {}
            }
            t.focus_sec += p.elapsed_sec;
            *equivalents.entry(p.session_id.as_str()).or_default() += p.fraction();
        }

        for s in sessions {
            let Some(date) = s.local_date else { continue };
            match s.status {
                SessionStatus::Completed => {
                    let t = stats.entry(date, s.tag_id.clone());
                    t.ses_completed += 1;
                    t.ses_total += 1.0;
                }
                SessionStatus::Cancelled => {
                    let eq = equivalents.get(s.id.as_str()).copied().unwrap_or(0.0);
                    let frac = (eq / s.target_pomodoros.max(1) as f64).min(1.0);
                    if frac > 0.0 {
                        let t = stats.entry(date, s.tag_id.clone());
                        t.ses_incomplete += 1;
                        t.ses_total += frac;
                    }
                }
                SessionStatus::Active => {}
            }
        }
        stats
    }

    fn entry(&mut self, date: NaiveDate, tag: Option<String>) -> &mut Totals {
        self.days.entry(date).or_default().entry(tag).or_default()
    }

    pub fn day(&self, date: NaiveDate, filter: &TagFilter) -> Totals {
        self.range(date, date, filter)
    }

    /// Totals for `from..=to`.
    pub fn range(&self, from: NaiveDate, to: NaiveDate, filter: &TagFilter) -> Totals {
        let mut out = Totals::default();
        for per_tag in self.days.range(from..=to).map(|(_, v)| v) {
            for (tag, t) in per_tag {
                if filter.matches(tag) {
                    out.add(t);
                }
            }
        }
        out
    }

    pub fn all_time(&self, filter: &TagFilter) -> Totals {
        match (self.first_date(), self.last_date()) {
            (Some(a), Some(b)) => self.range(a, b, filter),
            _ => Totals::default(),
        }
    }

    pub fn month(&self, year: i32, month: u32, filter: &TagFilter) -> Totals {
        let (from, to) = month_bounds(year, month);
        self.range(from, to, filter)
    }

    pub fn year(&self, year: i32, filter: &TagFilter) -> Totals {
        self.range(ymd(year, 1, 1), ymd(year, 12, 31), filter)
    }

    /// Per-tag breakdown for a period, sorted by total pomodoros (largest first).
    pub fn by_tag(&self, from: NaiveDate, to: NaiveDate) -> Vec<(Option<String>, Totals)> {
        let mut acc: BTreeMap<Option<String>, Totals> = BTreeMap::new();
        for per_tag in self.days.range(from..=to).map(|(_, v)| v) {
            for (tag, t) in per_tag {
                acc.entry(tag.clone()).or_default().add(t);
            }
        }
        let mut out: Vec<_> = acc.into_iter().filter(|(_, t)| !t.is_empty()).collect();
        out.sort_by(|a, b| b.1.pom_total.total_cmp(&a.1.pom_total));
        out
    }

    pub fn first_date(&self) -> Option<NaiveDate> {
        self.days.keys().next().copied()
    }

    pub fn last_date(&self) -> Option<NaiveDate> {
        self.days.keys().next_back().copied()
    }

    /// Years that have any recorded activity, newest first.
    pub fn years(&self) -> Vec<i32> {
        let mut ys: Vec<i32> = self.days.keys().map(|d| d.year()).collect();
        ys.dedup();
        ys.reverse();
        ys
    }

    /// The goal in force on a day (falls back to the spec defaults when history is empty).
    pub fn goal_on(&self, date: NaiveDate) -> GoalEntry {
        goal_for(&self.goals, date).cloned().unwrap_or(GoalEntry {
            effective_from: date,
            goal_sessions: 2,
            pomodoros_per_session: 4,
        })
    }

    pub fn heat_level_on(&self, date: NaiveDate, filter: &TagFilter) -> u8 {
        heat_level(self.day(date, filter).pom_total, self.goal_on(date).goal_pomodoros() as f64)
    }

    fn meets_goal(&self, date: NaiveDate, filter: &TagFilter) -> bool {
        self.day(date, filter).ses_completed >= self.goal_on(date).goal_sessions
    }

    /// (current, longest) streaks of consecutive days meeting the session goal. The current
    /// streak still counts if today is not done yet but yesterday met the goal.
    pub fn streaks(&self, today: NaiveDate, filter: &TagFilter) -> (u32, u32) {
        let mut longest = 0;
        let mut run = 0;
        let mut prev: Option<NaiveDate> = None;
        for &date in self.days.keys() {
            if self.meets_goal(date, filter) {
                run = if prev.is_some_and(|p| p + TimeDelta::days(1) == date) { run + 1 } else { 1 };
                prev = Some(date);
                longest = longest.max(run);
            }
        }
        let mut current = 0;
        let mut d = if self.meets_goal(today, filter) { today } else { today - TimeDelta::days(1) };
        while self.meets_goal(d, filter) {
            current += 1;
            d -= TimeDelta::days(1);
        }
        (current, longest)
    }

    /// The day with the most total pomodoros.
    pub fn best_day(&self, filter: &TagFilter) -> Option<(NaiveDate, Totals)> {
        self.days
            .keys()
            .map(|&d| (d, self.day(d, filter)))
            .filter(|(_, t)| t.pom_total > 0.0)
            .max_by(|a, b| a.1.pom_total.total_cmp(&b.1.pom_total).then(b.0.cmp(&a.0)))
    }
}

/// Heatmap intensity 0–4 for `total` pomodoros against a goal of `goal` pomodoros.
pub fn heat_level(total: f64, goal: f64) -> u8 {
    if total <= 0.0 {
        0
    } else if total < 0.25 * goal {
        1
    } else if total < 0.5 * goal {
        2
    } else if total < goal {
        3
    } else {
        4
    }
}

/// `Tue 6 Oct 2026 — 8 completed, 1 incomplete, 8.4 total · 2 sessions · 3h 28m focus`
pub fn day_tooltip(date: NaiveDate, t: &Totals) -> String {
    format!(
        "{} — {} completed, {} incomplete, {} total · {} session{} · {} focus",
        date.format("%a %-d %b %Y"),
        t.pom_completed,
        t.pom_incomplete,
        fmt_decimal(t.pom_total),
        t.ses_completed,
        if t.ses_completed == 1 { "" } else { "s" },
        fmt_focus(t.focus_sec),
    )
}

pub fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

pub fn month_bounds(year: i32, month: u32) -> (NaiveDate, NaiveDate) {
    let from = ymd(year, month, 1);
    let next = if month == 12 { ymd(year + 1, 1, 1) } else { ymd(year, month + 1, 1) };
    (from, next - TimeDelta::days(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pom(id: &str, session: &str, date: NaiveDate, elapsed: f64, completed: bool) -> Pomodoro {
        Pomodoro {
            id: id.into(),
            session_id: session.into(),
            started_at: String::new(),
            ended_at: String::new(),
            planned_sec: 1500,
            elapsed_sec: elapsed,
            status: if completed { PomodoroStatus::Completed } else { PomodoroStatus::Cancelled },
            local_date: date,
        }
    }

    fn ses(id: &str, tag: Option<&str>, date: NaiveDate, status: SessionStatus) -> Session {
        Session {
            id: id.into(),
            tag_id: tag.map(Into::into),
            mode: Mode::Standard,
            target_pomodoros: 4,
            started_at: String::new(),
            ended_at: None,
            status,
            local_date: Some(date),
            goal_sessions: Some(2),
        }
    }

    #[test]
    fn heat_levels_against_default_goal() {
        let g = 8.0;
        assert_eq!(heat_level(0.0, g), 0);
        assert_eq!(heat_level(1.9, g), 1);
        assert_eq!(heat_level(2.0, g), 2);
        assert_eq!(heat_level(4.0, g), 3);
        assert_eq!(heat_level(7.9, g), 3);
        assert_eq!(heat_level(8.0, g), 4);
    }

    #[test]
    fn tooltip_format() {
        let t = Totals {
            pom_completed: 8,
            pom_incomplete: 1,
            pom_total: 8.4,
            ses_completed: 2,
            focus_sec: 3.0 * 3600.0 + 28.0 * 60.0,
            ..Totals::default()
        };
        assert_eq!(
            day_tooltip(ymd(2026, 10, 6), &t),
            "Tue 6 Oct 2026 — 8 completed, 1 incomplete, 8.4 total · 2 sessions · 3h 28m focus"
        );
    }

    #[test]
    fn filters_streaks_and_best_day() {
        let d1 = ymd(2026, 10, 5);
        let d2 = ymd(2026, 10, 6);
        let mut sessions = vec![];
        let mut poms = vec![];
        for (i, d) in [(0, d1), (1, d1), (2, d2), (3, d2)] {
            let sid = format!("s{i}");
            let tag = if i % 2 == 0 { Some("chem") } else { None };
            sessions.push(ses(&sid, tag, d, SessionStatus::Completed));
            for j in 0..4 {
                poms.push(pom(&format!("p{i}{j}"), &sid, d, 1500.0, true));
            }
        }
        let goals = vec![GoalEntry { effective_from: d1, goal_sessions: 2, pomodoros_per_session: 4 }];
        let st = Stats::build(&sessions, &poms, goals);
        assert_eq!(st.all_time(&TagFilter::All).ses_completed, 4);
        assert_eq!(st.all_time(&TagFilter::Tag("chem".into())).pom_completed, 8);
        assert_eq!(st.all_time(&TagFilter::Untagged).pom_completed, 8);
        assert_eq!(st.heat_level_on(d2, &TagFilter::All), 4);
        assert_eq!(st.heat_level_on(d2, &TagFilter::Untagged), 3);
        assert_eq!(st.streaks(ymd(2026, 10, 7), &TagFilter::All), (2, 2));
        assert_eq!(st.streaks(ymd(2026, 10, 8), &TagFilter::All), (0, 2));
        assert_eq!(st.streaks(d2, &TagFilter::Untagged), (0, 0));
        assert_eq!(st.best_day(&TagFilter::All).unwrap().0, d1);
        assert_eq!(st.month(2026, 10, &TagFilter::All).pom_total, 16.0);
        assert_eq!(st.years(), vec![2026]);
        assert_eq!(st.by_tag(d1, d2).len(), 2);
    }

    #[test]
    fn day_goal_history_is_respected() {
        let d1 = ymd(2026, 1, 1);
        let d2 = ymd(2026, 2, 1);
        let goals = vec![
            GoalEntry { effective_from: d1, goal_sessions: 1, pomodoros_per_session: 4 },
            GoalEntry { effective_from: d2, goal_sessions: 3, pomodoros_per_session: 4 },
        ];
        let st = Stats::build(&[], &[], goals);
        assert_eq!(st.goal_on(ymd(2026, 1, 15)).goal_pomodoros(), 4);
        assert_eq!(st.goal_on(ymd(2026, 3, 1)).goal_pomodoros(), 12);
        assert_eq!(st.goal_on(ymd(2025, 6, 1)).goal_pomodoros(), 4);
    }
}
