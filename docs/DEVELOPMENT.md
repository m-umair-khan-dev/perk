# Perk: developer guide

How Perk works under the hood, and how to build, test and package it.
For the user-facing overview, see the [README](../README.md).

## How it works

| | Windows | Linux |
|---|---|---|
| Keep awake | `SetThreadExecutionState(ES_CONTINUOUS \| ES_SYSTEM_REQUIRED [\| ES_DISPLAY_REQUIRED])` | systemd-logind `Inhibit("idle", block)` + the desktop's own API: GNOME `SessionManager.Inhibit`, or `org.freedesktop.ScreenSaver` / `org.freedesktop.PowerManagement` (KDE, Xfce, ...) |
| Lid events | `RegisterPowerSettingNotification(GUID_LIDSWITCH_STATE_CHANGE)` → `WM_POWERBROADCAST` | UPower `LidIsClosed` change signals (D-Bus) |
| Battery events | `GUID_ACDC_POWER_SOURCE` + `GUID_BATTERY_PERCENTAGE_REMAINING` notifications | UPower `OnBattery` + `DisplayDevice.Percentage` change signals |
| Sleep events | `PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC` | logind `PrepareForSleep` signal |
| Idle time ("when away") | `GetLastInputInfo` | GNOME Mutter `IdleMonitor.GetIdletime` (other desktops: unavailable) |
| Idle timeouts ("when away") | Active power plan: `STANDBYIDLE` / `VIDEOIDLE` (AC or DC) | `gsettings`: `sleep-inactive-{ac,battery}-{type,timeout}`, `org.gnome.desktop.session idle-delay` |
| Crash cleanup | Execution state is per-thread; the OS clears it when the process ends | The logind lock is a file descriptor and desktop cookies are tied to the D-Bus connection; both vanish when the process ends |
| Autostart | `HKCU\...\CurrentVersion\Run` | `~/.config/autostart/perk.desktop` |

### Battery safety, in layers

1. Perk only ever asks the OS to *reset idle timers*. It never takes a lock that can block
   lid-close or user-requested sleep (no logind `sleep` / `handle-lid-switch` locks; on
   Windows, `SetThreadExecutionState` cannot block lid or explicit sleep).
2. On top of that, Perk drops its request as soon as the lid closes or the system starts
   to suspend, and refuses to re-acquire while the lid is closed (even if you click
   "Keep awake").
3. Timed sessions use wall-clock time: a 1-hour session that spent 3 hours asleep has
   expired when the machine wakes.
4. If Perk crashes or is killed, the OS releases everything automatically.

If the lid state cannot be observed (Linux without UPower), the menu shows
"⚠ Lid detection unavailable". Lid-close suspend still works thanks to layer 1.

Note: if you have configured your OS to "do nothing" when the lid closes, Perk still
releases its request, but whether the machine then sleeps is up to your OS settings.

### "Start timer when I step away"

A timed session with this option is a *when away* session (`Effective::Waiting` until it
needs the OS lock):

- The deadline is always *last user input + duration*, so it keeps moving while you work
  and only runs out after that much continuous idle time.
- The OS lock is taken only once idle time reaches *sleep timeout − 1 minute* (with
  "Keep display on": the earlier of sleep and screen-off). Before that the OS would not
  sleep anyway. When you come back, the lock is released again.
- Sleep set to *never*: Perk never needs to step in. Timeouts unreadable: Perk steps in
  immediately (safe side). Idle time unavailable: the session is a normal timer.
- The app samples idle time and timeouts at each wake-up (at most once a minute while a
  session runs, plus exactly at the step-in moment), so there is no extra polling thread.

## Hardware compatibility

Perk checks what each machine supports at startup and logs it, for example
`power capabilities: lid=true modern_standby=true s3_sleep=false hibernate=false battery=true`.

| Machine | Behaviour |
|---|---|
| Laptop with **Modern Standby** (S0ix; most laptops since ~2019, e.g. Dell Latitude 7420) | Lid close releases keep-awake immediately (verified). Windows sends no "suspend" message on these machines; the lid event is what matters. |
| Laptop with classic **S3 sleep** / **hibernate** | Lid close releases immediately; suspend/resume are also tracked. |
| **Desktop** / no lid | Lid monitoring is skipped ("no lid"); keep-awake works normally. |
| No battery, or a desktop on a UPS | Low-battery pause only applies while running on battery power. |
| **Docked with lid closed** (external monitor) | Perk stays paused while the lid is closed, as designed. Whether Windows/Linux keeps running is decided by your OS lid setting. |
| Windows on **ARM** | The x64 build runs under Windows' built-in emulation. |
| Linux without UPower | Lid/battery unavailable (shown in the menu); lid-close sleep still works because Perk never blocks it. |

## Settings

Most settings are in the tray menu. The file is at `%APPDATA%\perk\config.toml` (Windows)
or `~/.config/perk/config.toml` (Linux):

```toml
keep_display_on = true          # also prevent screen dimming / lock
resume_after_lid_open = true    # continue the session after lid reopen / wake
custom_minutes = 45             # the "Custom" duration (1 - 10080)
last_minutes = 0                # what the on/off toggle uses (0 = indefinitely)
pause_below_battery_percent = 10  # pause on battery at/below this charge (0 = off)
emergency_hotkey = "Ctrl+Alt+Shift+K"   # empty string disables it
start_timer_when_away = false   # timed sessions count only while you are idle
```

Use **Settings → Reload settings** after editing. Logs: `%LOCALAPPDATA%\perk\perk.log`
or `~/.local/state/perk/perk.log` (set `PERK_LOG=debug` for more detail).

## Building (Docker only, nothing installed on the host)

All compilation, testing and packaging runs in a container. Host requirement:
**Docker with Compose** (Docker Desktop on Windows). The project folder is mounted into
the container; the Rust toolchain, crates and build output live in the image and in
named Docker volumes.

```sh
docker compose build                        # build the toolchain image (once)
docker compose run --rm dev test            # run the test suite
docker compose run --rm dev lint            # rustfmt + clippy (Linux and Windows targets)
docker compose run --rm dev build           # release builds for both platforms
docker compose run --rm dev package         # everything below, into ./dist
docker compose run --rm dev help            # all commands
```

`package` produces in `./dist`:

| File | What |
|---|---|
| `perk-<v>-windows-x86_64-setup.exe` | Per-user installer (no admin), Start-menu shortcut, uninstaller |
| `perk-<v>-windows-x86_64.zip` | Portable `perk.exe` |
| `perk_<v>-1_amd64.deb` | Debian/Ubuntu package |
| `perk-<v>-1.x86_64.rpm` | Fedora/openSUSE package (tested on Fedora 44) |
| `perk-<v>-linux-x86_64.tar.gz` | Portable Linux binary + `.desktop` + icon |

The Windows build is cross-compiled with MinGW-w64; the finished executables need no
Docker and no runtime installs.

To remove everything Docker-side: `docker compose down --volumes --rmi local`.

### Host-side requirements (unavoidable)

- **GUI testing** needs a real desktop, so it happens on the host by running the built
  binary: on Windows just run `perk.exe` from `./dist`; nothing gets installed.
  The container has no display and no system D-Bus, so it runs unit tests only.
- **Linux runtime**: GTK 3 and `libayatana-appindicator3-1` (the `.deb` pulls them in),
  plus UPower for lid detection (installed by default on desktop distributions). GNOME
  needs the *AppIndicator* extension to show tray icons (Ubuntu ships it by default).
  For the custom-duration dialog, `zenity` or `kdialog`; without them Perk opens the
  settings file instead.
- **Windows runtime**: Windows 10 1607 or later. Nothing else.

## Software centre listing (Linux)

The `.deb` installs `packaging/linux/io.github.m_umair_khan_dev.perk.metainfo.xml` to
`/usr/share/metainfo/`. Ubuntu App Center, GNOME Software and KDE Discover read it for the
description and screenshots. Screenshots must be public URLs, so they point at
`assets/screenshots/*.png` on the `main` branch of GitHub. The PNGs are rendered from the
SVGs next to them (1600x900). After changing the file, check it with
`appstreamcli validate packaging/linux/*.metainfo.xml`.

## Project layout

```
src/
  core/        state machine (state.rs), controller, duration parsing (pure, unit-tested)
  platform/    Inhibitor trait + windows/ and linux/ implementations
  icon/        SDF icon renderer (also used by build.rs for the .exe icon)
  app.rs       tray, menu and event loop (tao + tray-icon)
  config.rs    TOML settings
docker/        dev image
scripts/dev.sh container entry point (build/test/lint/package)
packaging/     .desktop, SVG icon, AppStream metainfo, NSIS installer script
assets/        README images; screenshots/ is what software centres show
```

The core state machine takes the current time as an argument and never calls the OS,
so every transition (lid close mid-session, expiry while asleep, emergency stop while
paused, ...) is covered by fast deterministic tests. The controller is tested against a
fake inhibitor to verify that the OS lock is released on lid close, sleep, expiry and
emergency stop.
