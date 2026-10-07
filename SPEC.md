# Pomodoro for GNOME — Specification

Version 0.3 · 2026-10-07

## 1. Overview

A small, native GNOME Pomodoro timer for Arch Linux: 25-minute focus, 5-minute break, four pomodoros to a session. Sessions can be tagged (chemistry, mathematics, ...), and a GitHub-style heatmap shows everything completed. It runs offline, stores data locally, and follows the system dark/light setting.

**Goals**

- Target idle memory under 80 MB, near-zero CPU while the timer runs
- Looks and behaves like a first-party GNOME app (GTK4 + libadwaita)
- Full history stays with the user: local storage, one-click export and import
- Nothing is lost silently: an in-progress phase survives closing the app, crashes and power loss (restored paused), and cancelled work is recorded

**Non-goals (v1)**

- No accounts, cloud sync, or telemetry
- No task lists or per-task tracking (tags are the only categorisation)
- No background mode or tray: closing the app quits it and pauses the timer (see 4.5)
- Not tuned for non-GNOME desktops (it should still run on KDE)

## 2. Platform and stack

| Concern | Choice | Why |
| --- | --- | --- |
| Language | Rust | Small binary, low RAM, no interpreter |
| UI toolkit | GTK4 + libadwaita (`gtk4-rs`, `libadwaita-rs`) | Native look, automatic dark/light via `AdwStyleManager` |
| Timer | Elapsed time accumulated from a monotonic clock, checkpointed to the database every 5 s; UI redraws once per second | Immune to wall-clock changes; recoverable after a crash |
| Storage | SQLite (`rusqlite`) in `~/.local/share/<app-id>/` | One file, atomic writes, easy range queries for the heatmap |
| Settings | TOML file in `~/.config/<app-id>/` | Simple, easy to back up |
| Audio | GStreamer or `libcanberra` playing a bundled sound file | Already present on GNOME systems |
| Notifications | `GNotification` via `GApplication` | Works with GNOME Shell's notification center |
| App ID | `io.github.<you>.Pomodoro` | Needed for GNOME integration and a later Flatpak |

**Clock handling.** While a phase runs, elapsed time accumulates from a monotonic clock (`CLOCK_MONOTONIC`, which does not advance during system suspend), so wall-clock changes and NTP corrections cannot distort it. The elapsed time is checkpointed to the database every 5 seconds and on every state change. Wall-clock timestamps are used only for records (`started_at`, `ended_at`) and day attribution.

## 3. Concepts and definitions

- **Pomodoro**: one focus phase. It ends as *completed* (ran to zero) or *cancelled* (user cancelled it).
- **Session**: a group of pomodoros, 4 by default. It ends as *completed* (4 pomodoros completed) or *cancelled* (user cancelled it).
- **Tag**: an optional label on a session (e.g. Chemistry). Every pomodoro in the session inherits it.
- **Mode**: *Standard* or *Blitz*, chosen per session (section 4.4).
- **Elapsed focus time**: seconds of focus actually run. Paused time is excluded.
- **Fraction** of a pomodoro = elapsed focus time / planned focus duration (planned duration is stored with each pomodoro, so later settings changes never distort history). A completed pomodoro is 1.0.

## 4. Timer

### 4.1 Durations (all editable)

| Setting | Default | Allowed range |
| --- | --- | --- |
| Focus | 25 min | 1–120 min |
| Short break | 5 min | 1–60 min |
| Long break | 15 min | 1–90 min |
| Pomodoros per session | 4 | 1–12 |

Changing durations affects the next phase only, never the running one. A running session keeps the pomodoro count it started with.

### 4.2 Controls

| Action | Available when | Effect |
| --- | --- | --- |
| Start | Idle, or between phases | Starts the next phase. With no active session, it starts a new session using the chosen tag and mode. |
| Pause / Resume | Any running phase | Freezes or resumes the remaining time. Pause is also applied automatically when the app closes and when the computer suspends (see 4.5). |
| Cancel pomodoro | During a focus phase | Ends the pomodoro as cancelled and saves its elapsed fraction. The session stays active and waits for the user to start another pomodoro. |
| Cancel session | Any time a session is active | Cancels the running pomodoro (if any, saved as above), then ends the session as cancelled and saves its progress. |
| Skip break | During a short or long break (Standard mode) | Ends the break and moves to the next focus phase, ready to start. |

- Both cancel actions open a confirmation dialog stating what will be saved, e.g. `Cancel this pomodoro? 2.5 min of focus (0.1 pomodoro) will be saved as incomplete.`
- There is no Skip or Reset for focus phases: a pomodoro either completes or is explicitly cancelled.
- Keyboard: `Space` start/pause, `Ctrl+Backspace` cancel pomodoro, `Ctrl+Shift+Backspace` cancel session, `N` skip break, `Ctrl+,` preferences.
- Auto-start of the next phase is a toggle, off by default (Blitz mode overrides it for focus phases, see 4.4).
- The window title shows the remaining time, e.g. `18:42 · Focus`.

### 4.3 Sessions

Standard mode: Focus 1 → short break → Focus 2 → short break → Focus 3 → short break → Focus 4 → long break. After the long break the session is already complete, and the next Start begins a new one.

- The main screen shows the session position as dots (filled = completed pomodoro; the current one shows live progress).
- A session is completed at the moment its 4th pomodoro completes. The long break after it does not affect the record.
- A cancelled pomodoro does not fill a slot: the session still needs 4 completed pomodoros.
- A session never expires on its own. If the app was closed for days with a session active, the session is still there, shown with a hint such as `Started 3 days ago`, until the user finishes or cancels it.

### 4.4 Blitz pomodoro

Blitz is a session mode in which short breaks are skipped automatically.

- A **Blitz** switch sits next to the tag picker before a session starts. Its default comes from preferences (off by default). The mode is fixed for the session; to change it, cancel and start a new session.
- In a Blitz session, short breaks never happen: when focus 1, 2 or 3 completes, the chime plays, the notification reads `Pomodoro 2 of 4 done — next one started`, and the next focus phase starts immediately without waiting for input.
- The **long break after the 4th pomodoro is kept** and works as in Standard mode.
- Pause and Cancel work as usual. Cancelling a pomodoro in Blitz mode stops the automatic chain; the session waits for the user.
- Blitz sessions count exactly like Standard sessions in all statistics. The mode is stored on the session record.

### 4.5 Pause on close and crash recovery

**Principle:** the timer counts only while the app is open and the computer is awake. In every other situation the phase is paused and waits for the user. Nothing is completed or cancelled automatically.

**Normal close.** Closing the window pauses any running phase (focus or break), saves the exact remaining time, and quits. There is no confirmation dialog. On the next launch the same phase is shown paused with the same remaining time, and the user presses Resume. Because there is no background mode, the chime and notifications fire only while the app is running (the window may be minimised or on another workspace).

**System suspend.** Suspending the computer (detected through the logind `PrepareForSleep` signal) pauses the running phase. On wake the app shows a toast `Timer paused while the computer was asleep` and waits for Resume. Locking the screen does not pause the timer.

**Crash, kill or power loss.**

- The active state is written on every state change (phase start, pause, resume, cancel, completion) and, while a phase is running, checkpointed every 5 seconds (`phase_elapsed_sec`, `checkpointed_at`).
- Each checkpoint overwrites the single `active_state` row in place, so checkpoints leave no history and database size does not grow with them. The SQLite WAL file is folded back into the main database automatically (auto-checkpoint), so it stays at a few MB at most.
- A `clean_exit` flag in the database is cleared when the app starts and set only after a normal close. If the flag is not set at launch, the previous run ended unexpectedly.
- After an unexpected end, the app restores the pomodoro and session from the last checkpoint, **paused**, and shows a banner: `The app closed unexpectedly. Your pomodoro was restored; up to 5 seconds of progress may be lost.`
- Pomodoro and session records are committed in a single transaction together with the state change, so each one either lands completely or not at all. SQLite runs in WAL mode with `synchronous=FULL`, so a committed record survives power loss.

| State at the moment the app stopped | After a normal close or suspend | After a crash or power loss |
| --- | --- | --- |
| Focus or break running | Paused with exact remaining time | Paused with remaining time as of the last checkpoint (at most 5 s lost) |
| Paused | Paused, same remaining time | Paused, same remaining time |
| Waiting between phases | Waiting, unchanged | Waiting, unchanged (state is written at each transition) |
| Pomodoro reaching zero at the instant of the crash | Not applicable | Either its completion was committed (recorded as completed, app waits) or it was not (restored paused with a few seconds left) |
| Session active, no phase running | Unchanged | Unchanged |

- A recovered phase is never completed or cancelled automatically, however much time has passed since.
- Already recorded pomodoros and sessions are never lost or altered by a crash.
- In a Blitz session, Resume continues the chain as normal.
- If the database cannot be opened, the app shows an error naming the file and offers to restore the newest automatic backup if one exists. It never overwrites or deletes the file itself.

## 5. Tags

- Each session has at most one tag. Untagged sessions are allowed.
- The tag is picked from a dropdown before a session starts, with `New tag…` at the bottom. A preference remembers the last used tag as the default.
- The tag can be changed while the session is active. The change applies to the whole session, including its already recorded pomodoros.
- Tag fields: name (1–32 characters, unique ignoring case) and a colour from a fixed palette of 8 libadwaita named colours, so it reads correctly in light and dark themes.
- Rename updates the tag everywhere (history references the tag by ID).
- Deleting a tag archives it: it disappears from the picker, but history keeps its name and colour. Archived tags can be restored from preferences.
- The tag appears as a coloured dot plus name on the main screen and in statistics.

## 6. Daily goal

The default goal is 2 completed sessions per day (8 pomodoros with default settings), editable from 1 to 10.

- The main screen shows `1 / 2 sessions today`. Completed sessions fill the progress ring solidly; the live progress of the current session is shown as a lighter segment.
- Only completed sessions count towards the goal. Cancelled ones do not.
- Reaching the goal triggers one subtle acknowledgement (a brief checkmark state and a line in the notification) and nothing more. There is no nagging afterwards.
- Days split at local midnight by default; a preference sets a custom day-start hour (for example 04:00).
- The goal in force on each day is stored. Changing it applies from that day forward and never rewrites history.

## 7. Progress tracking

### 7.1 Counting rules

Every pomodoro and session is stored with a status. Cancelled items count as incomplete, and their progress is kept as a decimal.

| Measure | Complete | Incomplete | Total |
| --- | --- | --- | --- |
| Pomodoros | Number completed | Number cancelled with elapsed time > 0 | Completed + sum of fractions of cancelled pomodoros |
| Sessions | Number completed | Number cancelled with progress > 0 | Completed + sum of fractions of cancelled sessions |

- **Session fraction** (for a cancelled session) = pomodoro-equivalents in that session / pomodoros per session, where pomodoro-equivalents = completed pomodoros + fractions of its cancelled pomodoros. A completed session is exactly 1.0, even if extra cancelled attempts happened inside it. A cancelled session's fraction is capped at 1.0.
- A pomodoro cancelled with no elapsed time, or a session cancelled with no progress, is not stored.
- A pomodoro or session belongs to the date on which it ended (completed or cancelled), after applying the day-start hour.
- Focus time = sum of elapsed focus seconds across all pomodoros, completed and cancelled.
- Values are shown rounded to 1 decimal place (half up). Seconds are stored exactly; tooltips and exports carry full precision.

**Worked examples** (4 pomodoros per session, 25 min focus)

| Scenario | Pomodoros (C / I / Total) | Sessions (C / I / Total) |
| --- | --- | --- |
| Pomodoro 1 completed, pomodoro 2 cancelled after 2.5 min, then the session is cancelled | 1 / 1 / 1.1 | 0 / 1 / 0.275 (shown 0.3) |
| Pomodoros 1–3 completed, pomodoro 4 cancelled after 12.5 min, then the session is cancelled | 3 / 1 / 3.5 | 0 / 1 / 0.875 (shown 0.9) |
| Four pomodoros completed in a row | 4 / 0 / 4 | 1 / 0 / 1 |

### 7.2 Heatmap

- Grid of small rounded squares: columns are weeks, rows are weekdays (week start per locale), one square per day.
- The heatmap counts **pomodoros**: the intensity of a day is its total pomodoros (completed plus fractional cancelled), not sessions.
- Default range is the last 12 months. A year selector switches to any earlier year that has data.
- Five levels, scaled to that day's goal in pomodoros (G = goal sessions × pomodoros per session, so G = 8 by default):

| Level | Total pomodoros that day (t) |
| --- | --- |
| 0 | t = 0 |
| 1 | 0 < t < 0.25 G (under 2 by default) |
| 2 | 0.25 G ≤ t < 0.5 G |
| 3 | 0.5 G ≤ t < G |
| 4 | t ≥ G (goal reached in pomodoros) |

- Hover or keyboard-focus a square for a tooltip: `Tue 6 Oct 2026 — 8 completed, 1 incomplete, 8.4 total · 2 sessions · 3h 28m focus`.
- Colours come from the libadwaita accent colour as 4 steps of intensity over a neutral surface colour for empty days. When a tag filter is active, the heatmap uses that tag's colour. Intensity never relies on hue alone.
- Month labels on top, weekday labels (Mon/Wed/Fri) on the left. Clicking a square selects that day in the summary.

### 7.3 Statistics view

- A tag filter (All / specific tag / Untagged) applies to the heatmap and every figure on the page.
- Summary cards for Day (selected square), Month, Year, and All time, each showing:

| Figure | Detail |
| --- | --- |
| Pomodoros | complete / incomplete / total |
| Sessions | complete / incomplete / total |
| Focus time | hours and minutes |

- A per-tag breakdown table for the selected period (tag, pomodoros total, sessions complete, focus time).
- Current streak (consecutive days meeting the session goal), longest streak, best day.
- Month and year pickers step back to any prior period.
- All figures are recomputed from stored records, never from cached totals.

## 8. Sound and notifications

When any phase ends, the app plays a soft, short chime in the spirit of an elevator ding: a clean two-note "ding-dong" or a single bell tone, about 1 second long, with a gentle attack and natural decay.

- The sound file is bundled (OGG/Opus or WAV, under 50 KB), synthesised for the project or CC0-licensed.
- The same chime plays for focus end and break end by default. An optional lower variant can be set for break end.
- In Blitz mode the chime still plays at the end of each focus phase, since there is no break to mark the transition.
- Volume slider, mute toggle, and a Test sound button in preferences. Default volume is about 50%.
- A GNOME desktop notification accompanies the chime, e.g. `Focus complete — take a 5 min break`. Clicking it raises the window.
- Do Not Disturb silences the notification banner. The in-app chime follows its own toggle.

## 9. GNOME integration and theming

- **Dark/light:** `AdwStyleManager` with the system colour scheme, following changes live with no restart. Preferences offer an override: System / Light / Dark.
- **Colours:** libadwaita named colours (`accent_bg_color`, `window_bg_color`, `card_bg_color`) and the libadwaita palette everywhere, including the heatmap and tag colours. No hard-coded hex values for UI chrome.
- **Widgets:** `AdwApplicationWindow`, `AdwHeaderBar`, `AdwPreferencesWindow` (or `AdwPreferencesDialog`), `AdwToastOverlay` for confirmations like "Export saved".
- **Window:** compact by default (about 360 × 520 px), resizable, adaptive down to phone width with `AdwBreakpoint`.
- **Icons:** symbolic icons for actions, a scalable app icon plus a symbolic variant.
- **Desktop integration:** `.desktop` file with correct `StartupWMClass`, AppStream `metainfo.xml`, app ID matching the D-Bus name.
- **Single instance:** a second launch raises the existing window via `GApplication`.
- **Accessibility:** all controls labelled for Orca, full keyboard navigation, respect for the system reduced-motion setting.

## 10. Data model, export and import

### 10.1 Tables

| Table | Key fields |
| --- | --- |
| `tags` | `id` (UUID), `name`, `color`, `archived` |
| `sessions` | `id`, `tag_id` (nullable), `mode` (`standard` / `blitz`), `target_pomodoros`, `started_at`, `ended_at`, `status` (`active` / `completed` / `cancelled`), `local_date`, `goal_sessions` (goal in force that day) |
| `pomodoros` | `id`, `session_id`, `started_at`, `ended_at`, `planned_sec`, `elapsed_sec`, `status` (`completed` / `cancelled`), `local_date` |
| `active_state` | single row: current session and pomodoro IDs, phase type, run state (`running` / `paused` / `waiting`), `phase_planned_sec`, `phase_elapsed_sec` (checkpointed every 5 s), `checkpointed_at`, `clean_exit` |

Fractions, daily totals, streaks and heatmap values are derived from `pomodoros` and `sessions`. The tag of a pomodoro is the tag of its session.

### 10.2 Export

- Menu → Export data writes one `.json` file through the GTK file chooser (portal-friendly). Default name: `pomodoro-backup-2026-10-07.json`.
- Contents: format version, export timestamp, settings, goal history, tags (including archived), sessions, pomodoros. The active in-progress state is not exported.
- Optional CSV export: one row per pomodoro with columns `date, tag, session_id, session_status, mode, status, planned_min, elapsed_min, fraction`.

### 10.3 Import

- Menu → Import data reads a JSON backup, validating the format version, schema and references (every pomodoro points to a session, every session to an existing or included tag). A bad file shows an error toast and changes nothing.
- A preview shows record counts and the date range before anything is applied.
- **Merge** (default): adds tags, sessions and pomodoros whose `id` is not present, so repeated imports are safe. Tags that arrive with a new ID but the same name (ignoring case) as an existing tag are mapped onto the existing tag.
- **Replace**: wipes current history first, after a confirmation dialog. It is disabled while a session is active; cancel the session first.
- Settings are restored only in Replace mode, or when "Also import settings" is ticked.
- The whole import runs in one database transaction: it applies fully or not at all.

### 10.4 Safety

- Records are written to the database immediately at each completion or cancellation (WAL mode, `synchronous=FULL`), so a crash or power loss does not lose finished work. In-progress state recovery is described in 4.5.
- Optional automatic rolling backup (last 7 daily JSON snapshots in the data folder), off by default.

## 11. UI layout

**Timer view**

- Large countdown in the centre, phase label above it (Focus / Short break / Long break), a circular progress ring around the digits.
- Session dots below the timer.
- Before a session: tag dropdown and Blitz switch under the dots. During a session they show the current tag and mode, with the tag still changeable.
- Primary Start/Pause button; secondary actions in a menu or row: Cancel pomodoro, Cancel session, Skip break.
- Footer: `Today: 1 / 2 sessions · 5 pomodoros`.
- Header bar: view switcher (Timer | Statistics) and main menu (Preferences, Export, Import, Keyboard shortcuts, About).

**Statistics view:** see 7.3.

**Preferences**

| Group | Settings |
| --- | --- |
| Timer | Focus, short break, long break, pomodoros per session, auto-start next phase |
| Session | Default mode (Standard / Blitz), remember last tag |
| Goal | Sessions per day (default 2), day-start hour |
| Tags | Add, rename, recolour, archive, restore |
| Sound | Enable, volume, test, optional separate break sound |
| Notifications | Enable desktop notifications |
| Appearance | System / Light / Dark |
| Data | Export, Import, open data folder, auto-backup |

## 12. Packaging for Arch

- A `PKGBUILD` that builds from source (`cargo`, with runtime dependencies `gtk4`, `libadwaita`, `gstreamer` or `libcanberra`, `sqlite`), installable locally with `makepkg -si`.
- Optional later: AUR package and Flatpak manifest.
- Installs the binary, `.desktop` file, icons, sound file and AppStream metadata under `/usr`.

## 13. Acceptance criteria

- [ ] A fresh install starts with 25/5 timers, 4 pomodoros per session, a goal of 2 sessions, and Standard mode.
- [ ] Changing any duration persists after restart and applies from the next phase.
- [ ] Four completed pomodoros produce one completed session and a long break.
- [ ] In Blitz mode, short breaks never occur, the next focus starts immediately with a chime, and the long break after the 4th pomodoro still happens.
- [ ] A session can be tagged and re-tagged; statistics and heatmap filter correctly by tag, including Untagged.
- [ ] Cancelling a pomodoro 2.5 min into a 25 min focus records 0.1 incomplete; with 1 earlier completed pomodoro the totals read 1 / 1 / 1.1, and cancelling the session records 0 / 1 / 0.275.
- [ ] A pomodoro or session cancelled with zero progress leaves no record.
- [ ] Closing the app mid-phase pauses it, and reopening shows the same phase paused with the same remaining time, whether the phase was focus or break.
- [ ] After killing the process (`kill -9`) or losing power mid-phase, relaunch restores the pomodoro and session paused, with at most 5 s of progress lost and all earlier records intact.
- [ ] Neither a pomodoro nor a session is ever cancelled automatically, and nothing completes while the app is closed.
- [ ] The heatmap intensity follows total pomodoros, and the tooltip shows completed, incomplete and total.
- [ ] Day, month, year and all-time figures match the stored records.
- [ ] Switching the GNOME appearance setting changes the app theme, tag colours and heatmap live.
- [ ] Export followed by Replace-import on a clean profile reproduces identical statistics, tags included.
- [ ] Importing the same file twice in Merge mode creates no duplicates.
- [ ] A soft chime plays at the end of each phase; mute and volume work.
- [ ] Suspending the computer pauses the running phase, and sleep time never counts as focus time.
- [ ] Idle memory stays under 80 MB.

## 14. Milestones

1. Core timer, phases, sound, persistence of the active state
2. Sessions, tags, Blitz mode, cancel logic, SQLite storage
3. Statistics view with heatmap and tag filter
4. Export/import, preferences, theming polish
5. PKGBUILD and release

## 15. Decisions and assumptions

**Decided**

- Language: Rust.
- Closing the app pauses the session. After a crash or power loss the pomodoro and session are restored paused from the last checkpoint.
- Pomodoros and sessions end only by completion or explicit cancellation, never automatically.
- Cancelled work counts as incomplete with a decimal fraction.
- No background mode or tray in v1.
- The heatmap counts pomodoros, not sessions, and its intensity uses total pomodoros including fractions.
- In Blitz mode the long break after the 4th pomodoro is kept, and the next focus starts with no pause after each chime.
- A cancelled pomodoro does not fill a session slot, so a session needs 4 completed pomodoros.
- System suspend pauses the running phase, and the user presses Resume after waking.
- The checkpoint interval is 5 seconds (up to 5 seconds of progress lost on a crash). Checkpoints overwrite one row, so they take no extra disk space.

**Assumptions to confirm**

- None open.
