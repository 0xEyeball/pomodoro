# Pomodoro

A small, native GNOME Pomodoro timer (GTK4 + libadwaita, Rust). 25-minute focus, 5-minute
break, four pomodoros to a session, tags, a daily goal and a heatmap of everything completed.
Fully offline; history lives in a local SQLite database. See [SPEC.md](SPEC.md).

## Layout

| Path | What |
| --- | --- |
| `core/` | Toolkit-independent library: timer engine, SQLite storage, statistics, export/import. All the rules from the spec live here and are unit-tested. |
| `app/` | The GTK4/libadwaita application. |
| `data/` | `.desktop` file, AppStream metadata, icons and the chime sounds. |
| `packaging/arch/PKGBUILD` | Arch package built from this checkout. |
| `tools/gen_chime.py` | Regenerates the bundled chimes (synthesised, no third-party audio). |

## Build and run

Arch:

```sh
sudo pacman -S --needed rust gtk4 libadwaita sqlite gst-plugins-base gst-plugins-good
cargo run -p pomodoro
```

Install as a package:

```sh
cd packaging/arch && makepkg -si
```

Ubuntu 24.04 / WSL (needs libadwaita ≥ 1.5):

```sh
sudo apt install pkg-config libgtk-4-dev libadwaita-1-dev libsqlite3-dev \
  libgtk-4-media-gstreamer gstreamer1.0-plugins-good
cargo run -p pomodoro
```

Tests (no GTK needed): `cargo test -p pomodoro-core` (add `--features bundled-sqlite` on a box
without the SQLite development package).

## Where things are stored

| What | Path |
| --- | --- |
| History database | `~/.local/share/io.github._0xEyeball.Pomodoro/pomodoro.db` |
| Automatic backups (if enabled) | `~/.local/share/io.github._0xEyeball.Pomodoro/backups/` |
| Settings | `~/.config/io.github._0xEyeball.Pomodoro/settings.toml` |

The app ID uses `_0xEyeball` because D-Bus names cannot have an element that starts with a digit.

## Implementation notes

- **Timer:** elapsed time comes from `CLOCK_MONOTONIC` (Rust `Instant`), which ignores wall-clock
  changes and does not advance during suspend. The UI wakes only when the displayed second
  changes and only while a phase runs. The active state is checkpointed every 5 s and on every
  state change; records and state change are committed in one transaction (WAL,
  `synchronous=FULL`).
- **Rendering:** the app defaults to GTK's cairo renderer (`GSK_RENDERER=cairo`) to keep idle
  memory low; set `GSK_RENDERER` yourself to override.
- **Week start** for the heatmap comes from the locale (glibc `nl_langinfo`), like GtkCalendar.
- **AppStream:** the `<developer>` element has no `id`, because a reverse-DNS id cannot contain
  `0xEyeball`. Add one (e.g. your own domain) before submitting to Flathub.

## Manual test checklist (Arch + GNOME)

These depend on a real GNOME session and were not verifiable under WSL:

- [ ] `makepkg -si` builds, the check step passes, and the app appears in the GNOME app grid with its icon.
- [ ] The running window is grouped under the right icon in the dock / Alt+Tab.
- [ ] A second launch raises the existing window.
- [ ] A phase end plays the chime; mute and volume work; "Test Sound" plays.
- [ ] A phase end shows a GNOME notification; clicking it raises the window; Do Not Disturb hides the banner.
- [ ] `systemctl suspend` mid-focus: after waking the timer is paused, the toast appears, and the elapsed time did not include the sleep.
- [ ] Switching Settings → Appearance → Style (light/dark) changes the app, tag colours and heatmap live; changing the accent colour changes the ring and heatmap.
- [ ] Tag picker: "New tag…" creates and selects a tag; re-tagging during a session updates Statistics; archiving hides it from the picker.
- [ ] Export, then Import → Replace on a fresh profile gives identical statistics; importing the same file twice in Merge mode adds nothing.
- [ ] Idle memory (`grep VmRSS /proc/$(pgrep -x pomodoro)/status`) is under 80 MB.
- [ ] Orca reads the timer, buttons and heatmap squares.

Verified under WSL (Ubuntu 24.04, WSLg): start/pause with Space, cancel dialog, close-and-reopen
restores the paused phase, `kill -9` restores from the last checkpoint with the banner, phase
completion, skip break with `N`, session completion with the goal checkmark, statistics and
heatmap, preferences, dark style override, idle RSS 57 MB with the cairo renderer.
