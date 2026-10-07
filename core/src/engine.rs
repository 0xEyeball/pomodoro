//! The timer state machine.
//!
//! Elapsed time is accumulated from the monotonic clock while a phase runs and checkpointed to
//! the database every [`CHECKPOINT_SEC`] seconds and on every state change. Records and the
//! active state are always written in the same transaction.

use std::time::Duration;

use chrono::{DateTime, FixedOffset, NaiveDate};

use crate::db::{self, goal_for, Db, Result};
use crate::model::*;
use crate::settings::Settings;
use crate::time::{fmt_ts, local_date, Clock};

pub const CHECKPOINT_SEC: u64 = 5;

/// What the UI needs to draw the timer.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Current phase, or the next one while waiting. `None` when idle.
    pub phase: Option<PhaseKind>,
    pub run_state: RunState,
    pub planned_sec: u32,
    pub elapsed_sec: f64,
    pub session: Option<SessionView>,
}

impl Snapshot {
    pub fn remaining_sec(&self) -> f64 {
        (self.planned_sec as f64 - self.elapsed_sec).max(0.0)
    }

    pub fn is_idle(&self) -> bool {
        self.phase.is_none() && self.run_state == RunState::Waiting
    }

    pub fn in_phase(&self) -> bool {
        self.phase.is_some() && self.run_state != RunState::Waiting
    }

    pub fn in_focus(&self) -> bool {
        self.in_phase() && self.phase == Some(PhaseKind::Focus)
    }

    pub fn progress(&self) -> f64 {
        if self.planned_sec == 0 {
            0.0
        } else {
            (self.elapsed_sec / self.planned_sec as f64).clamp(0.0, 1.0)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionView {
    pub id: String,
    pub tag_id: Option<String>,
    pub mode: Mode,
    pub target: u32,
    pub completed: u32,
    /// Fraction of the pomodoro currently in progress (0 when none).
    pub current_fraction: f64,
    /// Pomodoro-equivalents so far: completed + cancelled fractions + current fraction.
    pub equivalents: f64,
    pub started_at: String,
}

impl SessionView {
    pub fn fraction(&self) -> f64 {
        (self.equivalents / self.target.max(1) as f64).min(1.0)
    }
}

/// Emitted when a phase runs to zero.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub finished: PhaseKind,
    pub next: Option<PhaseKind>,
    /// The next phase started immediately (Blitz chain or auto-start).
    pub next_started: bool,
    pub next_planned_sec: u32,
    pub mode: Mode,
    pub completed_in_session: u32,
    pub target: u32,
    pub session_completed: bool,
    /// This completion made today's completed sessions reach the goal.
    pub goal_reached: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Today {
    pub date: NaiveDate,
    pub completed_sessions: u32,
    pub completed_pomodoros: u32,
    pub goal_sessions: u32,
}

pub struct Engine {
    db: Db,
    clock: Box<dyn Clock>,
    settings: Settings,
    /// Persisted state; while running, `phase_elapsed_sec` is the elapsed time at `running_since`.
    st: ActiveState,
    running_since: Option<Duration>,
    session: Option<Session>,
    completed: u32,
    cancelled_equivalents: f64,
    last_checkpoint: Duration,
}

impl Engine {
    /// Loads the persisted state. A phase that was running when the app last stopped is restored
    /// paused. The flag is true when the previous run ended without a normal close while a
    /// session or phase was in progress.
    pub fn new(db: Db, clock: Box<dyn Clock>, settings: Settings) -> Result<(Engine, bool)> {
        let mut st = db.active_state()?;
        let unexpected = !st.clean_exit && (st.session_id.is_some() || st.phase.is_some());
        if st.run_state == RunState::Running {
            st.run_state = RunState::Paused;
        }
        st.clean_exit = false;
        let last_checkpoint = clock.mono();
        let mut engine = Engine {
            db,
            clock,
            settings: settings.clamped(),
            st,
            running_since: None,
            session: None,
            completed: 0,
            cancelled_equivalents: 0.0,
            last_checkpoint,
        };
        engine.reload_session()?;
        engine.db.save_active_state(&engine.st)?;
        engine.ensure_goal()?;
        Ok((engine, unexpected))
    }

    fn reload_session(&mut self) -> Result<()> {
        self.session = match &self.st.session_id {
            Some(id) => self.db.session(id)?.filter(|s| s.status == SessionStatus::Active),
            None => None,
        };
        if self.session.is_none() && self.st.session_id.is_some() {
            // The referenced session vanished (e.g. replaced by an import): fall back to idle,
            // unless a long break (which has no session) is in progress.
            self.st.session_id = None;
            if self.st.phase != Some(PhaseKind::LongBreak) {
                self.st = ActiveState { clean_exit: self.st.clean_exit, ..ActiveState::default() };
            }
        }
        self.completed = 0;
        self.cancelled_equivalents = 0.0;
        if let Some(s) = &self.session {
            for p in self.db.session_pomodoros(&s.id)? {
                match p.status {
                    PomodoroStatus::Completed => self.completed += 1,
                    PomodoroStatus::Cancelled => self.cancelled_equivalents += p.fraction(),
                }
            }
        }
        Ok(())
    }

    // ---- accessors ---------------------------------------------------------

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn db_mut(&mut self) -> &mut Db {
        &mut self.db
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn now(&self) -> DateTime<FixedOffset> {
        self.clock.now()
    }

    pub fn today_date(&self) -> NaiveDate {
        local_date(self.clock.now(), self.settings.day_start_hour)
    }

    fn elapsed(&self) -> f64 {
        let running = self
            .running_since
            .map(|since| self.clock.mono().saturating_sub(since).as_secs_f64())
            .unwrap_or(0.0);
        // Millisecond precision is plenty and keeps stored values tidy.
        ((self.st.phase_elapsed_sec + running) * 1000.0).round() / 1000.0
    }

    fn current_focus_fraction(&self) -> f64 {
        if self.st.phase == Some(PhaseKind::Focus) && self.st.run_state != RunState::Waiting {
            fraction(self.elapsed(), self.st.phase_planned_sec)
        } else {
            0.0
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let elapsed = if self.st.run_state == RunState::Waiting { 0.0 } else { self.elapsed() };
        let planned = match (self.st.run_state, self.st.phase) {
            (RunState::Waiting, Some(kind)) => self.settings.phase_sec(kind),
            _ => self.st.phase_planned_sec,
        };
        let current_fraction = self.current_focus_fraction();
        Snapshot {
            phase: self.st.phase,
            run_state: self.st.run_state,
            planned_sec: planned,
            elapsed_sec: elapsed.min(planned as f64),
            session: self.session.as_ref().map(|s| SessionView {
                id: s.id.clone(),
                tag_id: s.tag_id.clone(),
                mode: s.mode,
                target: s.target_pomodoros,
                completed: self.completed,
                current_fraction,
                equivalents: self.completed as f64 + self.cancelled_equivalents + current_fraction,
                started_at: s.started_at.clone(),
            }),
        }
    }

    pub fn today(&self) -> Result<Today> {
        let date = self.today_date();
        Ok(Today {
            date,
            completed_sessions: self.db.completed_sessions_on(date)?,
            completed_pomodoros: self.db.completed_pomodoros_on(date)?,
            goal_sessions: self.goal_on(date)?.goal_sessions,
        })
    }

    fn goal_on(&self, date: NaiveDate) -> Result<GoalEntry> {
        let history = self.db.goal_history()?;
        Ok(goal_for(&history, date).cloned().unwrap_or(GoalEntry {
            effective_from: date,
            goal_sessions: self.settings.goal_sessions,
            pomodoros_per_session: self.settings.pomodoros_per_session,
        }))
    }

    /// Records today's goal if it differs from the one in force (never rewrites past days).
    pub fn ensure_goal(&mut self) -> Result<()> {
        let today = self.today_date();
        let history = self.db.goal_history()?;
        let current = goal_for(&history, today);
        let wanted = (self.settings.goal_sessions, self.settings.pomodoros_per_session);
        let differs = current.is_none_or(|g| {
            g.effective_from > today || (g.goal_sessions, g.pomodoros_per_session) != wanted
        });
        if differs {
            self.db.set_goal(&GoalEntry {
                effective_from: today,
                goal_sessions: wanted.0,
                pomodoros_per_session: wanted.1,
            })?;
        }
        Ok(())
    }

    /// Applies new settings. Durations affect the next phase only; a running session keeps its
    /// pomodoro count.
    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        self.settings = settings.clamped();
        self.ensure_goal()
    }

    // ---- persistence helpers ----------------------------------------------

    /// The state as it should be written right now (elapsed folded in, timestamped).
    fn persisted(&self, st: &ActiveState) -> ActiveState {
        let mut out = st.clone();
        out.checkpointed_at = Some(fmt_ts(self.clock.now()));
        out
    }

    fn save(&mut self) -> Result<()> {
        let mut st = self.st.clone();
        st.phase_elapsed_sec = self.elapsed();
        let st = self.persisted(&st);
        self.db.save_active_state(&st)?;
        self.last_checkpoint = self.clock.mono();
        Ok(())
    }

    /// Commits `next` as the new state together with `records` in one transaction.
    fn commit(
        &mut self,
        next: ActiveState,
        running: bool,
        records: impl FnOnce(&rusqlite::Connection) -> Result<()>,
    ) -> Result<()> {
        let to_write = self.persisted(&next);
        self.db.tx(|c| {
            records(c)?;
            db::save_active_state(c, &to_write)
        })?;
        self.st = next;
        self.running_since = running.then(|| self.clock.mono());
        self.last_checkpoint = self.clock.mono();
        Ok(())
    }

    fn phase_state(&self, kind: PhaseKind, run_state: RunState, session_id: Option<String>) -> ActiveState {
        let in_phase = run_state != RunState::Waiting;
        let focus = in_phase && kind == PhaseKind::Focus;
        ActiveState {
            session_id,
            pomodoro_id: focus.then(db::new_id),
            pomodoro_started_at: focus.then(|| fmt_ts(self.clock.now())),
            phase: Some(kind),
            run_state,
            phase_planned_sec: if in_phase { self.settings.phase_sec(kind) } else { 0 },
            phase_elapsed_sec: 0.0,
            checkpointed_at: None,
            clean_exit: false,
        }
    }

    fn idle_state(&self) -> ActiveState {
        ActiveState { clean_exit: false, ..ActiveState::default() }
    }

    fn session_id(&self) -> Option<String> {
        self.session.as_ref().map(|s| s.id.clone())
    }

    // ---- controls ----------------------------------------------------------

    /// Starts the next phase; with no active session, starts a new one with `tag` and `mode`.
    pub fn start(&mut self, tag: Option<String>, mode: Mode) -> Result<()> {
        if self.st.run_state != RunState::Waiting {
            return Ok(());
        }
        match self.st.phase {
            None => {
                let session = Session {
                    id: db::new_id(),
                    tag_id: tag,
                    mode,
                    target_pomodoros: self.settings.pomodoros_per_session,
                    started_at: fmt_ts(self.clock.now()),
                    ended_at: None,
                    status: SessionStatus::Active,
                    local_date: None,
                    goal_sessions: None,
                };
                let next = self.phase_state(PhaseKind::Focus, RunState::Running, Some(session.id.clone()));
                let s = session.clone();
                self.commit(next, true, |c| db::insert_session(c, &s))?;
                self.session = Some(session);
                self.completed = 0;
                self.cancelled_equivalents = 0.0;
            }
            Some(kind) => {
                let next = self.phase_state(kind, RunState::Running, self.session_id());
                self.commit(next, true, |_| Ok(()))?;
            }
        }
        Ok(())
    }

    /// Pauses a running phase. Returns true if something was paused.
    pub fn pause(&mut self) -> Result<bool> {
        if self.st.run_state != RunState::Running {
            return Ok(false);
        }
        self.st.phase_elapsed_sec = self.elapsed();
        self.st.run_state = RunState::Paused;
        self.running_since = None;
        self.save()?;
        Ok(true)
    }

    pub fn resume(&mut self) -> Result<()> {
        if self.st.run_state != RunState::Paused {
            return Ok(());
        }
        self.st.run_state = RunState::Running;
        self.running_since = Some(self.clock.mono());
        self.save()
    }

    /// Space: start when waiting, otherwise pause / resume.
    pub fn toggle(&mut self, tag: Option<String>, mode: Mode) -> Result<()> {
        match self.st.run_state {
            RunState::Waiting => self.start(tag, mode),
            RunState::Running => self.pause().map(|_| ()),
            RunState::Paused => self.resume(),
        }
    }

    /// Elapsed seconds and fraction that cancelling the current pomodoro would save.
    pub fn cancel_pomodoro_preview(&self) -> Option<(f64, f64)> {
        let snap = self.snapshot();
        snap.in_focus().then(|| (snap.elapsed_sec, snap.progress()))
    }

    fn cancelled_pomodoro(&self) -> Option<Pomodoro> {
        if self.st.phase != Some(PhaseKind::Focus) || self.st.run_state == RunState::Waiting {
            return None;
        }
        let elapsed = self.elapsed().min(self.st.phase_planned_sec as f64);
        if elapsed <= 0.0 {
            return None;
        }
        let now = self.clock.now();
        Some(Pomodoro {
            id: self.st.pomodoro_id.clone().unwrap_or_else(db::new_id),
            session_id: self.session_id()?,
            started_at: self.st.pomodoro_started_at.clone().unwrap_or_else(|| fmt_ts(now)),
            ended_at: fmt_ts(now),
            planned_sec: self.st.phase_planned_sec,
            elapsed_sec: elapsed,
            status: PomodoroStatus::Cancelled,
            local_date: local_date(now, self.settings.day_start_hour),
        })
    }

    /// Ends the current pomodoro as cancelled. The session stays active, waiting for Start.
    /// Returns the saved fraction (0 when nothing was stored).
    pub fn cancel_pomodoro(&mut self) -> Result<Option<f64>> {
        if self.cancel_pomodoro_preview().is_none() {
            return Ok(None);
        }
        let record = self.cancelled_pomodoro();
        let saved = record.as_ref().map(Pomodoro::fraction).unwrap_or(0.0);
        let next = self.phase_state(PhaseKind::Focus, RunState::Waiting, self.session_id());
        let r = record.clone();
        self.commit(next, false, |c| r.as_ref().map_or(Ok(()), |p| db::insert_pomodoro(c, p)))?;
        self.cancelled_equivalents += saved;
        Ok(Some(saved))
    }

    /// Pomodoro-equivalents and session fraction that cancelling the session would save.
    pub fn cancel_session_preview(&self) -> Option<(f64, f64)> {
        self.snapshot().session.map(|s| (s.equivalents, s.fraction()))
    }

    /// Cancels the running pomodoro (if any), then ends the session as cancelled.
    /// A session with no progress leaves no record.
    pub fn cancel_session(&mut self) -> Result<bool> {
        let Some(session) = self.session.clone() else { return Ok(false) };
        let pomodoro = self.cancelled_pomodoro();
        let equivalents = self.completed as f64
            + self.cancelled_equivalents
            + pomodoro.as_ref().map(Pomodoro::fraction).unwrap_or(0.0);
        let now = self.clock.now();
        let date = local_date(now, self.settings.day_start_hour);
        let goal = self.goal_on(date)?.goal_sessions;
        let next = self.idle_state();
        self.commit(next, false, |c| {
            if let Some(p) = &pomodoro {
                db::insert_pomodoro(c, p)?;
            }
            if equivalents > 0.0 {
                db::finish_session(c, &session.id, SessionStatus::Cancelled, &fmt_ts(now), date, goal)
            } else {
                db::delete_session(c, &session.id)
            }
        })?;
        self.session = None;
        self.completed = 0;
        self.cancelled_equivalents = 0.0;
        Ok(true)
    }

    pub fn can_skip_break(&self) -> bool {
        self.st.phase.is_some_and(PhaseKind::is_break)
    }

    /// Ends the current (or upcoming) break; the next focus phase waits for Start.
    pub fn skip_break(&mut self) -> Result<()> {
        if !self.can_skip_break() {
            return Ok(());
        }
        let next = match self.session_id() {
            Some(id) => self.phase_state(PhaseKind::Focus, RunState::Waiting, Some(id)),
            None => self.idle_state(),
        };
        self.commit(next, false, |_| Ok(()))
    }

    /// Changes the tag of the active session (and so of its recorded pomodoros).
    pub fn set_session_tag(&mut self, tag: Option<String>) -> Result<()> {
        if let Some(s) = &mut self.session {
            self.db.set_session_tag(&s.id, tag.as_deref())?;
            s.tag_id = tag;
        }
        Ok(())
    }

    /// Called about once a second. Completes the phase when it reaches zero and checkpoints
    /// the elapsed time every few seconds.
    pub fn tick(&mut self) -> Result<Option<Completion>> {
        if self.st.run_state != RunState::Running {
            return Ok(None);
        }
        if self.elapsed() >= self.st.phase_planned_sec as f64 {
            return self.complete().map(Some);
        }
        if self.clock.mono().saturating_sub(self.last_checkpoint) >= Duration::from_secs(CHECKPOINT_SEC) {
            self.save()?;
        }
        Ok(None)
    }

    /// Seconds until the running phase ends, for scheduling an exact wake-up.
    pub fn remaining_sec(&self) -> Option<f64> {
        (self.st.run_state == RunState::Running)
            .then(|| (self.st.phase_planned_sec as f64 - self.elapsed()).max(0.0))
    }

    fn complete(&mut self) -> Result<Completion> {
        let kind = self.st.phase.unwrap_or(PhaseKind::Focus);
        let now = self.clock.now();
        let date = local_date(now, self.settings.day_start_hour);
        let auto = self.settings.auto_start;
        let mode = self.session.as_ref().map(|s| s.mode).unwrap_or(Mode::Standard);
        let target = self.session.as_ref().map(|s| s.target_pomodoros).unwrap_or(0);

        let mut pomodoro = None;
        let mut session_completed = false;
        let (next_kind, next_started) = match kind {
            PhaseKind::Focus => {
                pomodoro = self.session.as_ref().map(|s| Pomodoro {
                    id: self.st.pomodoro_id.clone().unwrap_or_else(db::new_id),
                    session_id: s.id.clone(),
                    started_at: self.st.pomodoro_started_at.clone().unwrap_or_else(|| fmt_ts(now)),
                    ended_at: fmt_ts(now),
                    planned_sec: self.st.phase_planned_sec,
                    elapsed_sec: self.st.phase_planned_sec as f64,
                    status: PomodoroStatus::Completed,
                    local_date: date,
                });
                let completed = self.completed + pomodoro.is_some() as u32;
                if completed >= target {
                    session_completed = self.session.is_some();
                    (Some(PhaseKind::LongBreak), auto)
                } else if mode == Mode::Blitz {
                    (Some(PhaseKind::Focus), true)
                } else {
                    (Some(PhaseKind::ShortBreak), auto)
                }
            }
            PhaseKind::ShortBreak if self.session.is_some() => (Some(PhaseKind::Focus), auto),
            PhaseKind::ShortBreak | PhaseKind::LongBreak => (None, false),
        };

        let session_id = if session_completed { None } else { self.session_id() };
        let next = match next_kind {
            Some(k) => {
                let rs = if next_started { RunState::Running } else { RunState::Waiting };
                self.phase_state(k, rs, session_id)
            }
            None => self.idle_state(),
        };
        let goal = self.goal_on(date)?.goal_sessions;
        let finished_session = self.session.clone().filter(|_| session_completed);
        let p = pomodoro.clone();
        self.commit(next, next_kind.is_some() && next_started, |c| {
            if let Some(p) = &p {
                db::insert_pomodoro(c, p)?;
            }
            if let Some(s) = &finished_session {
                db::finish_session(c, &s.id, SessionStatus::Completed, &fmt_ts(now), date, goal)?;
            }
            Ok(())
        })?;

        if pomodoro.is_some() {
            self.completed += 1;
        }
        let completed_in_session = self.completed;
        let goal_reached = session_completed && self.db.completed_sessions_on(date)? == goal;
        if session_completed {
            self.session = None;
            self.completed = 0;
            self.cancelled_equivalents = 0.0;
        }
        Ok(Completion {
            finished: kind,
            next: next_kind,
            next_started,
            next_planned_sec: next_kind.map(|k| self.settings.phase_sec(k)).unwrap_or(0),
            mode,
            completed_in_session,
            target,
            session_completed,
            goal_reached,
        })
    }

    /// Normal close: pause whatever runs and mark the exit as clean.
    pub fn close(&mut self) -> Result<()> {
        self.pause()?;
        self.st.clean_exit = true;
        self.save()
    }

    /// Re-reads the session after the database was changed underneath (e.g. an import).
    pub fn reload(&mut self) -> Result<()> {
        self.reload_session()?;
        self.save()?;
        self.ensure_goal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{Stats, TagFilter};
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone)]
    struct FakeClock {
        mono: Rc<Cell<Duration>>,
        wall: Rc<Cell<DateTime<FixedOffset>>>,
    }

    impl FakeClock {
        fn new() -> Self {
            FakeClock {
                mono: Rc::new(Cell::new(Duration::ZERO)),
                wall: Rc::new(Cell::new(
                    DateTime::parse_from_rfc3339("2026-10-06T09:00:00+02:00").unwrap(),
                )),
            }
        }
        fn advance(&self, secs: f64) {
            self.mono.set(self.mono.get() + Duration::from_secs_f64(secs));
            self.wall.set(self.wall.get() + chrono::TimeDelta::milliseconds((secs * 1000.0) as i64));
        }
        /// Wall time passes but the monotonic clock does not (app closed or suspended).
        fn advance_wall_only(&self, secs: i64) {
            self.wall.set(self.wall.get() + chrono::TimeDelta::seconds(secs));
        }
    }

    impl Clock for FakeClock {
        fn mono(&self) -> Duration {
            self.mono.get()
        }
        fn now(&self) -> DateTime<FixedOffset> {
            self.wall.get()
        }
    }

    fn engine_with(clock: &FakeClock, settings: Settings) -> Engine {
        Engine::new(Db::open_in_memory().unwrap(), Box::new(clock.clone()), settings).unwrap().0
    }

    fn engine(clock: &FakeClock) -> Engine {
        engine_with(clock, Settings::default())
    }

    /// Runs the current phase to completion with ticks every second.
    fn run_out(e: &mut Engine, clock: &FakeClock) -> Completion {
        for _ in 0..100_000 {
            clock.advance(1.0);
            if let Some(c) = e.tick().unwrap() {
                return c;
            }
        }
        panic!("phase never completed");
    }

    fn stats(e: &Engine) -> Stats {
        Stats::load(e.db()).unwrap()
    }

    #[test]
    fn standard_session_with_long_break() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Standard).unwrap();
        for i in 1..=4 {
            let c = run_out(&mut e, &clock);
            assert_eq!(c.finished, PhaseKind::Focus);
            assert_eq!(c.completed_in_session, i);
            assert!(!c.next_started);
            if i < 4 {
                assert_eq!(c.next, Some(PhaseKind::ShortBreak));
                e.start(None, Mode::Standard).unwrap();
                assert_eq!(run_out(&mut e, &clock).next, Some(PhaseKind::Focus));
                e.start(None, Mode::Standard).unwrap();
            } else {
                assert!(c.session_completed);
                assert_eq!(c.next, Some(PhaseKind::LongBreak));
            }
        }
        // Session is already recorded as completed before the long break.
        let s = e.db().sessions().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].status, SessionStatus::Completed);
        assert!(e.snapshot().session.is_none());
        e.start(None, Mode::Standard).unwrap();
        assert_eq!(e.snapshot().phase, Some(PhaseKind::LongBreak));
        assert_eq!(e.snapshot().planned_sec, 15 * 60);
        let c = run_out(&mut e, &clock);
        assert_eq!(c.next, None);
        assert!(e.snapshot().is_idle());

        let t = stats(&e).all_time(&TagFilter::All);
        assert_eq!((t.pom_completed, t.ses_completed), (4, 1));
        assert_eq!(t.focus_sec, 4.0 * 25.0 * 60.0);
    }

    #[test]
    fn blitz_skips_short_breaks_but_keeps_long_break() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Blitz).unwrap();
        for i in 1..=3 {
            let c = run_out(&mut e, &clock);
            assert_eq!(c.next, Some(PhaseKind::Focus));
            assert!(c.next_started, "blitz must chain focus {i}");
            assert_eq!(e.snapshot().run_state, RunState::Running);
        }
        let c = run_out(&mut e, &clock);
        assert!(c.session_completed);
        assert_eq!(c.next, Some(PhaseKind::LongBreak));
        assert_eq!(e.db().sessions().unwrap()[0].mode, Mode::Blitz);
    }

    #[test]
    fn blitz_cancel_stops_chain() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Blitz).unwrap();
        clock.advance(60.0);
        e.cancel_pomodoro().unwrap();
        let s = e.snapshot();
        assert_eq!((s.phase, s.run_state), (Some(PhaseKind::Focus), RunState::Waiting));
        clock.advance(3600.0);
        assert!(e.tick().unwrap().is_none());
    }

    #[test]
    fn spec_example_cancel_fractions() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Standard).unwrap();
        run_out(&mut e, &clock);
        e.skip_break().unwrap();
        e.start(None, Mode::Standard).unwrap();
        clock.advance(150.0);
        let (elapsed, frac) = e.cancel_pomodoro_preview().unwrap();
        assert_eq!((elapsed, frac), (150.0, 0.1));
        assert_eq!(e.cancel_pomodoro().unwrap(), Some(0.1));

        let t = stats(&e).all_time(&TagFilter::All);
        assert_eq!((t.pom_completed, t.pom_incomplete), (1, 1));
        assert!((t.pom_total - 1.1).abs() < 1e-9);

        let (eq, f) = e.cancel_session_preview().unwrap();
        assert!((eq - 1.1).abs() < 1e-9 && (f - 0.275).abs() < 1e-9);
        e.cancel_session().unwrap();
        let t = stats(&e).all_time(&TagFilter::All);
        assert_eq!((t.ses_completed, t.ses_incomplete), (0, 1));
        assert!((t.ses_total - 0.275).abs() < 1e-9);
    }

    #[test]
    fn spec_example_three_and_a_half() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Blitz).unwrap();
        for _ in 0..3 {
            run_out(&mut e, &clock);
        }
        clock.advance(750.0);
        e.cancel_session().unwrap();
        let t = stats(&e).all_time(&TagFilter::All);
        assert_eq!((t.pom_completed, t.pom_incomplete), (3, 1));
        assert!((t.pom_total - 3.5).abs() < 1e-9);
        assert!((t.ses_total - 0.875).abs() < 1e-9);
    }

    #[test]
    fn zero_progress_leaves_no_record() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Standard).unwrap();
        e.pause().unwrap();
        assert_eq!(e.cancel_pomodoro().unwrap(), Some(0.0));
        assert!(e.db().pomodoros().unwrap().is_empty());
        e.cancel_session().unwrap();
        assert!(e.db().sessions().unwrap().is_empty());
        assert!(e.snapshot().is_idle());
    }

    #[test]
    fn cancelled_pomodoro_does_not_fill_slot() {
        let clock = FakeClock::new();
        let mut e = engine_with(&clock, Settings { pomodoros_per_session: 2, ..Settings::default() });
        e.start(None, Mode::Blitz).unwrap();
        clock.advance(600.0);
        e.cancel_pomodoro().unwrap();
        e.start(None, Mode::Blitz).unwrap();
        assert!(!run_out(&mut e, &clock).session_completed);
        let c = run_out(&mut e, &clock);
        assert!(c.session_completed);
        let t = stats(&e).all_time(&TagFilter::All);
        assert_eq!(t.ses_total, 1.0, "a completed session is exactly 1.0");
    }

    #[test]
    fn pause_excludes_time_and_close_restores_exactly() {
        let clock = FakeClock::new();
        let db_path = std::env::temp_dir().join(format!("pomo-{}.db", uuid::Uuid::new_v4()));
        let mut e =
            Engine::new(Db::open(&db_path).unwrap(), Box::new(clock.clone()), Settings::default()).unwrap().0;
        e.start(None, Mode::Standard).unwrap();
        clock.advance(100.0);
        e.pause().unwrap();
        clock.advance(500.0);
        e.resume().unwrap();
        clock.advance(23.5);
        e.close().unwrap();
        drop(e);

        clock.advance_wall_only(3 * 86400);
        let (e, unexpected) =
            Engine::new(Db::open(&db_path).unwrap(), Box::new(clock.clone()), Settings::default()).unwrap();
        assert!(!unexpected);
        let s = e.snapshot();
        assert_eq!((s.phase, s.run_state), (Some(PhaseKind::Focus), RunState::Paused));
        assert_eq!(s.elapsed_sec, 123.5);
        assert!(e.db().pomodoros().unwrap().is_empty(), "nothing completes while closed");
        std::fs::remove_file(&db_path).ok();
    }

    #[test]
    fn crash_restores_last_checkpoint_paused() {
        let clock = FakeClock::new();
        let db_path = std::env::temp_dir().join(format!("pomo-{}.db", uuid::Uuid::new_v4()));
        let mut e =
            Engine::new(Db::open(&db_path).unwrap(), Box::new(clock.clone()), Settings::default()).unwrap().0;
        e.start(None, Mode::Standard).unwrap();
        run_out(&mut e, &clock); // one completed pomodoro on record
        e.start(None, Mode::Standard).unwrap(); // short break
        run_out(&mut e, &clock);
        e.start(None, Mode::Standard).unwrap(); // focus 2
        for _ in 0..63 {
            clock.advance(1.0);
            e.tick().unwrap();
        }
        // Simulated kill -9: the engine is dropped without close().
        std::mem::forget(e);

        clock.advance_wall_only(7200);
        let (e, unexpected) =
            Engine::new(Db::open(&db_path).unwrap(), Box::new(clock.clone()), Settings::default()).unwrap();
        assert!(unexpected);
        let s = e.snapshot();
        assert_eq!((s.phase, s.run_state), (Some(PhaseKind::Focus), RunState::Paused));
        assert!(s.elapsed_sec >= 58.0 && s.elapsed_sec <= 63.0, "lost at most 5 s: {}", s.elapsed_sec);
        assert_eq!(s.session.unwrap().completed, 1);
        assert_eq!(e.db().pomodoros().unwrap().len(), 1);
        std::fs::remove_file(&db_path).ok();
    }

    #[test]
    fn suspend_time_never_counts() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Standard).unwrap();
        clock.advance(60.0);
        assert!(e.pause().unwrap()); // PrepareForSleep(true)
        clock.advance_wall_only(8 * 3600);
        e.resume().unwrap();
        assert_eq!(e.snapshot().elapsed_sec, 60.0);
    }

    #[test]
    fn durations_apply_to_next_phase_only() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        e.start(None, Mode::Standard).unwrap();
        e.set_settings(Settings { focus_min: 50, pomodoros_per_session: 2, ..Settings::default() }).unwrap();
        assert_eq!(e.snapshot().planned_sec, 25 * 60);
        assert_eq!(e.snapshot().session.unwrap().target, 4);
        run_out(&mut e, &clock);
        e.skip_break().unwrap();
        assert_eq!(e.snapshot().planned_sec, 50 * 60);
    }

    #[test]
    fn retag_applies_to_whole_session() {
        let clock = FakeClock::new();
        let mut e = engine(&clock);
        let chem = e.db_mut().create_tag("Chemistry", TagColor::Blue).unwrap();
        let maths = e.db_mut().create_tag("Mathematics", TagColor::Red).unwrap();
        e.start(Some(chem.id.clone()), Mode::Standard).unwrap();
        run_out(&mut e, &clock);
        e.set_session_tag(Some(maths.id.clone())).unwrap();
        let st = stats(&e);
        assert_eq!(st.all_time(&TagFilter::Tag(chem.id)).pom_completed, 0);
        assert_eq!(st.all_time(&TagFilter::Tag(maths.id)).pom_completed, 1);
        assert_eq!(st.all_time(&TagFilter::Untagged).pom_completed, 0);
    }

    #[test]
    fn goal_reached_once() {
        let clock = FakeClock::new();
        let mut e = engine_with(&clock, Settings { pomodoros_per_session: 1, goal_sessions: 2, ..Settings::default() });
        let mut reached = vec![];
        for _ in 0..3 {
            e.start(None, Mode::Standard).unwrap();
            reached.push(run_out(&mut e, &clock).goal_reached);
            e.skip_break().unwrap();
        }
        assert_eq!(reached, [false, true, false]);
        let today = e.today().unwrap();
        assert_eq!((today.completed_sessions, today.goal_sessions), (3, 2));
    }
}
