//! JSON export/import, CSV export and rolling automatic backups.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::db::{self, Db, Error, Result};
use crate::model::*;
use crate::settings::Settings;
use crate::time::fmt_ts;

pub const FORMAT_VERSION: u32 = 1;
pub const AUTO_BACKUP_KEEP: usize = 7;
const AUTO_BACKUP_PREFIX: &str = "pomodoro-backup-";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    pub format_version: u32,
    pub exported_at: String,
    pub settings: Settings,
    pub goal_history: Vec<GoalEntry>,
    pub tags: Vec<Tag>,
    pub sessions: Vec<Session>,
    pub pomodoros: Vec<Pomodoro>,
}

pub fn export(db: &Db, settings: &Settings, now: DateTime<FixedOffset>) -> Result<Backup> {
    Ok(Backup {
        format_version: FORMAT_VERSION,
        exported_at: fmt_ts(now),
        settings: settings.clone(),
        goal_history: db.goal_history()?,
        tags: db.tags()?,
        sessions: db.sessions()?,
        pomodoros: db.pomodoros()?,
    })
}

pub fn default_file_name(date: NaiveDate) -> String {
    format!("{AUTO_BACKUP_PREFIX}{date}.json")
}

pub fn to_json(b: &Backup) -> String {
    serde_json::to_string_pretty(b).expect("backup serialises")
}

/// One row per pomodoro.
pub fn to_csv(db: &Db) -> Result<String> {
    let tags: HashMap<String, String> = db.tags()?.into_iter().map(|t| (t.id, t.name)).collect();
    let sessions: HashMap<String, Session> = db.sessions()?.into_iter().map(|s| (s.id.clone(), s)).collect();
    let mut out = String::from("date,tag,session_id,session_status,mode,status,planned_min,elapsed_min,fraction\n");
    for p in db.pomodoros()? {
        let s = sessions.get(&p.session_id);
        let tag = s.and_then(|s| s.tag_id.as_ref()).and_then(|id| tags.get(id)).map(String::as_str);
        let row = [
            p.local_date.to_string(),
            csv_field(tag.unwrap_or("")),
            csv_field(&p.session_id),
            s.map(|s| s.status.as_str()).unwrap_or("").to_owned(),
            s.map(|s| s.mode.as_str()).unwrap_or("").to_owned(),
            p.status.as_str().to_owned(),
            (p.planned_sec as f64 / 60.0).to_string(),
            (p.elapsed_sec / 60.0).to_string(),
            p.fraction().to_string(),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    Ok(out)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Parses and validates a backup without touching the database.
pub fn parse(json: &str, db: &Db) -> Result<Backup> {
    let b: Backup = serde_json::from_str(json).map_err(|e| Error::Invalid(format!("Not a valid backup file: {e}")))?;
    validate(&b, db)?;
    Ok(b)
}

fn validate(b: &Backup, db: &Db) -> Result<()> {
    let invalid = |m: String| Err(Error::Invalid(m));
    if b.format_version != FORMAT_VERSION {
        return invalid(format!("Unsupported backup format version {}", b.format_version));
    }
    let mut tag_ids = HashSet::new();
    for t in &b.tags {
        db::validate_tag_name(&t.name)?;
        if t.id.is_empty() || !tag_ids.insert(t.id.as_str()) {
            return invalid(format!("Duplicate or empty tag id {:?}", t.id));
        }
    }
    let existing_tags: HashSet<String> = db.tags()?.into_iter().map(|t| t.id).collect();
    let mut session_ids = HashSet::new();
    for s in &b.sessions {
        if s.id.is_empty() || !session_ids.insert(s.id.as_str()) {
            return invalid(format!("Duplicate or empty session id {:?}", s.id));
        }
        if let Some(t) = &s.tag_id {
            if !tag_ids.contains(t.as_str()) && !existing_tags.contains(t) {
                return invalid(format!("Session {} refers to unknown tag {t}", s.id));
            }
        }
        if s.target_pomodoros == 0 {
            return invalid(format!("Session {} has no target pomodoros", s.id));
        }
        if s.status != SessionStatus::Active && s.local_date.is_none() {
            return invalid(format!("Finished session {} has no date", s.id));
        }
    }
    let mut pomodoro_ids = HashSet::new();
    for p in &b.pomodoros {
        if p.id.is_empty() || !pomodoro_ids.insert(p.id.as_str()) {
            return invalid(format!("Duplicate or empty pomodoro id {:?}", p.id));
        }
        if !session_ids.contains(p.session_id.as_str()) {
            return invalid(format!("Pomodoro {} refers to unknown session {}", p.id, p.session_id));
        }
        if p.planned_sec == 0 || !p.elapsed_sec.is_finite() || p.elapsed_sec < 0.0 {
            return invalid(format!("Pomodoro {} has invalid durations", p.id));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub tags: usize,
    pub sessions: usize,
    pub pomodoros: usize,
    pub first: Option<NaiveDate>,
    pub last: Option<NaiveDate>,
}

pub fn preview(b: &Backup) -> Preview {
    let dates = b
        .pomodoros
        .iter()
        .map(|p| p.local_date)
        .chain(b.sessions.iter().filter_map(|s| s.local_date));
    let (first, last) = dates.fold((None, None), |(lo, hi): (Option<NaiveDate>, Option<NaiveDate>), d| {
        (Some(lo.map_or(d, |l| l.min(d))), Some(hi.map_or(d, |h| h.max(d))))
    });
    Preview { tags: b.tags.len(), sessions: b.sessions.len(), pomodoros: b.pomodoros.len(), first, last }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    Merge,
    Replace,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportResult {
    pub tags: usize,
    pub sessions: usize,
    pub pomodoros: usize,
}

/// Applies a validated backup in a single transaction. Settings are not touched here; the
/// caller decides whether to apply `b.settings`.
pub fn apply(db: &mut Db, b: &Backup, mode: ImportMode) -> Result<ImportResult> {
    db.tx(|c| {
        if mode == ImportMode::Replace {
            c.execute_batch(
                "DELETE FROM pomodoros; DELETE FROM sessions; DELETE FROM tags; DELETE FROM goal_history;",
            )?;
        }
        let mut result = ImportResult::default();

        let existing = db::tags(c)?;
        let mut ids: HashSet<String> = existing.iter().map(|t| t.id.clone()).collect();
        let mut by_name: HashMap<String, String> =
            existing.into_iter().map(|t| (t.name.to_lowercase(), t.id)).collect();
        let mut tag_map: HashMap<&str, String> = HashMap::new();
        for t in &b.tags {
            if ids.contains(&t.id) {
                tag_map.insert(&t.id, t.id.clone());
            } else if let Some(id) = by_name.get(&t.name.to_lowercase()) {
                tag_map.insert(&t.id, id.clone());
            } else {
                db::insert_tag(c, t)?;
                ids.insert(t.id.clone());
                by_name.insert(t.name.to_lowercase(), t.id.clone());
                tag_map.insert(&t.id, t.id.clone());
                result.tags += 1;
            }
        }

        for s in &b.sessions {
            if db::session(c, &s.id)?.is_some() {
                continue;
            }
            let mut s = s.clone();
            s.tag_id = s.tag_id.map(|t| tag_map.get(t.as_str()).cloned().unwrap_or(t));
            db::insert_session(c, &s)?;
            result.sessions += 1;
        }

        for p in &b.pomodoros {
            let exists: bool =
                c.query_row("SELECT EXISTS(SELECT 1 FROM pomodoros WHERE id = ?)", [&p.id], |r| r.get(0))?;
            if !exists {
                db::insert_pomodoro(c, p)?;
                result.pomodoros += 1;
            }
        }

        let have: HashSet<NaiveDate> = db::goal_history(c)?.into_iter().map(|g| g.effective_from).collect();
        for g in &b.goal_history {
            if !have.contains(&g.effective_from) {
                db::upsert_goal(c, g)?;
            }
        }
        Ok(result)
    })
}

/// Writes today's snapshot into `dir` (if not already there) and keeps the newest seven.
pub fn auto_backup(dir: &Path, db: &Db, settings: &Settings, now: DateTime<FixedOffset>, today: NaiveDate) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(default_file_name(today));
    if !path.exists() {
        let b = export(db, settings, now).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, to_json(&b))?;
        std::fs::rename(&tmp, &path)?;
    }
    let mut all = list_auto_backups(dir);
    while all.len() > AUTO_BACKUP_KEEP {
        std::fs::remove_file(all.remove(all.len() - 1))?;
    }
    Ok(())
}

/// Automatic backups in `dir`, newest first.
pub fn list_auto_backups(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(AUTO_BACKUP_PREFIX) && n.ends_with(".json"))
        })
        .collect();
    files.sort();
    files.reverse();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::stats::{Stats, TagFilter};
    use crate::time::SystemClock;

    fn sample_db() -> Db {
        let mut db = Db::open_in_memory().unwrap();
        let chem = db.create_tag("Chemistry", TagColor::Blue).unwrap();
        let old = db.create_tag("Old, \"quoted\"", TagColor::Red).unwrap();
        db.set_tag_archived(&old.id, true).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();
        db.tx(|c| {
            for (i, (tag, status)) in [
                (Some(chem.id.clone()), SessionStatus::Completed),
                (Some(old.id.clone()), SessionStatus::Cancelled),
                (None, SessionStatus::Cancelled),
            ]
            .into_iter()
            .enumerate()
            {
                let sid = format!("s{i}");
                db::insert_session(c, &Session {
                    id: sid.clone(),
                    tag_id: tag,
                    mode: Mode::Standard,
                    target_pomodoros: 4,
                    started_at: "2026-10-06T09:00:00.000+02:00".into(),
                    ended_at: Some("2026-10-06T11:00:00.000+02:00".into()),
                    status,
                    local_date: Some(d),
                    goal_sessions: Some(2),
                })?;
                let n = if status == SessionStatus::Completed { 4 } else { 1 };
                for j in 0..n {
                    db::insert_pomodoro(c, &Pomodoro {
                        id: format!("p{i}{j}"),
                        session_id: sid.clone(),
                        started_at: "2026-10-06T09:00:00.000+02:00".into(),
                        ended_at: "2026-10-06T09:25:00.000+02:00".into(),
                        planned_sec: 1500,
                        elapsed_sec: if n == 4 { 1500.0 } else { 150.0 },
                        status: if n == 4 { PomodoroStatus::Completed } else { PomodoroStatus::Cancelled },
                        local_date: d,
                    })?;
                }
            }
            db::upsert_goal(c, &GoalEntry { effective_from: d, goal_sessions: 2, pomodoros_per_session: 4 })
        })
        .unwrap();
        db
    }

    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-10-07T12:00:00+02:00").unwrap()
    }

    #[test]
    fn export_then_replace_reproduces_statistics() {
        let src = sample_db();
        let json = to_json(&export(&src, &Settings::default(), now()).unwrap());

        let mut dst = Db::open_in_memory().unwrap();
        let b = parse(&json, &dst).unwrap();
        let pv = preview(&b);
        assert_eq!((pv.tags, pv.sessions, pv.pomodoros), (2, 3, 6));
        apply(&mut dst, &b, ImportMode::Replace).unwrap();

        let (a, z) = (Stats::load(&src).unwrap(), Stats::load(&dst).unwrap());
        for f in [TagFilter::All, TagFilter::Untagged, TagFilter::Tag(src.tags().unwrap()[0].id.clone())] {
            assert_eq!(a.all_time(&f), z.all_time(&f));
        }
        assert_eq!(src.tags().unwrap(), dst.tags().unwrap());
        assert_eq!(src.goal_history().unwrap(), dst.goal_history().unwrap());
    }

    #[test]
    fn merge_twice_creates_no_duplicates_and_maps_tags_by_name() {
        let src = sample_db();
        let json = to_json(&export(&src, &Settings::default(), now()).unwrap());

        let mut dst = Db::open_in_memory().unwrap();
        let mine = dst.create_tag("chemistry", TagColor::Green).unwrap();
        let b = parse(&json, &dst).unwrap();
        let r1 = apply(&mut dst, &b, ImportMode::Merge).unwrap();
        assert_eq!(r1, ImportResult { tags: 1, sessions: 3, pomodoros: 6 });
        let r2 = apply(&mut dst, &b, ImportMode::Merge).unwrap();
        assert_eq!(r2, ImportResult::default());
        assert_eq!(dst.tags().unwrap().len(), 2);
        let s0 = dst.session("s0").unwrap().unwrap();
        assert_eq!(s0.tag_id, Some(mine.id));
    }

    #[test]
    fn bad_files_change_nothing() {
        let src = sample_db();
        let mut b = export(&src, &Settings::default(), now()).unwrap();
        b.pomodoros[0].session_id = "missing".into();
        let dst = Db::open_in_memory().unwrap();
        assert!(parse(&to_json(&b), &dst).is_err());
        assert!(parse("{ not json", &dst).is_err());
        let mut v = export(&src, &Settings::default(), now()).unwrap();
        v.format_version = 99;
        assert!(parse(&to_json(&v), &dst).is_err());
        assert!(dst.sessions().unwrap().is_empty());
    }

    #[test]
    fn csv_has_one_row_per_pomodoro() {
        let csv = to_csv(&sample_db()).unwrap();
        let lines: Vec<_> = csv.lines().collect();
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[0], "date,tag,session_id,session_status,mode,status,planned_min,elapsed_min,fraction");
        assert!(csv.contains("\"Old, \"\"quoted\"\"\""));
        assert!(csv.contains(",cancelled,25,2.5,0.1"));
    }

    #[test]
    fn auto_backup_keeps_seven() {
        let dir = std::env::temp_dir().join(format!("pomo-bk-{}", uuid::Uuid::new_v4()));
        let db = sample_db();
        for day in 1..=10 {
            let d = NaiveDate::from_ymd_opt(2026, 10, day).unwrap();
            auto_backup(&dir, &db, &Settings::default(), now(), d).unwrap();
        }
        let files = list_auto_backups(&dir);
        assert_eq!(files.len(), 7);
        assert!(files[0].ends_with("pomodoro-backup-2026-10-10.json"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn active_state_is_not_exported() {
        let (mut e, _) =
            Engine::new(Db::open_in_memory().unwrap(), Box::new(SystemClock::new()), Settings::default()).unwrap();
        e.start(None, Mode::Standard).unwrap();
        let json = to_json(&export(e.db(), e.settings(), now()).unwrap());
        assert!(!json.contains("active_state") && !json.contains("phase_elapsed"));
    }
}
