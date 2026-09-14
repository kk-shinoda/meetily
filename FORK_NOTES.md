# Fork notes (kk-shinoda/meetily)

Purpose: record automatically while a Google Meet / Microsoft Teams / Zoom call is running.
Everything fork-specific lives in new files; upstream files are touched in as few lines as possible so
`git merge upstream/main` stays cheap.

## Files added (no upstream counterpart)

| Path | Role |
| --- | --- |
| `frontend/src-tauri/src/meeting_detector/mod.rs` | Tauri inlined plugin `meeting-detector` (state, monitor task, command registration) |
| `frontend/src-tauri/src/meeting_detector/settings.rs` | Settings struct, persisted to `<app data>/meeting_detection.json` |
| `frontend/src-tauri/src/meeting_detector/signals.rs` | Probes: macOS Core Audio "process is capturing input" by bundle id; Zoom `CptHost` process |
| `frontend/src-tauri/src/meeting_detector/detector.rs` | Idle → Pending → Active state machine, start-confirm and stop-grace timers, recording start/stop |
| `frontend/src-tauri/src/meeting_detector/commands.rs` | `get_settings` / `set_settings` / `get_status` / `probe_signals` |
| `frontend/src/components/MeetingDetection/meetingDetectionService.ts` | `invoke('plugin:meeting-detector|…')` wrapper and types |
| `frontend/src/components/MeetingDetection/MeetingDetectionSettings.tsx` | Settings tab UI ("Auto Record") |
| `frontend/tests/lib/meeting-detection-service.test.ts` | Command names, event name, label table (bun test) |
| `frontend/tests/components/meeting-detection-settings.test.tsx` | Settings tab behaviour (bun test) |

## Upstream files touched (check these on every upstream merge)

| File | Change |
| --- | --- |
| `frontend/src-tauri/src/lib.rs` | `pub mod meeting_detector;` and `.plugin(meeting_detector::init())` (2 lines) |
| `frontend/src-tauri/build.rs` | `tauri_build::build()` → `try_build(...)` declaring the inlined plugin commands for the ACL |
| `frontend/src-tauri/tauri.conf.json` | `"meeting-detector:default"` permission; updater endpoint points at this fork's releases |
| `frontend/src/app/settings/page.tsx` | Import, one `TABS` entry, one `TabsContent` block |

If upstream changes the recording entry points, re-check these call sites in `detector.rs`:
`audio::recording_commands::{is_recording, start_recording_with_meeting_name, stop_recording, RecordingArgs}`,
`tray::{update_tray_menu, set_tray_state, RecordingState}`, and the `recording-stop-complete` event that
`RecordingPostProcessingProvider` consumes for the SQLite save.

## How detection works

1. Every `poll_interval_secs` (default 3 s) the monitor lists Core Audio client processes and keeps those
   with `is_running_input == true`, excluding Meetily's own pid. Bundle ids are mapped:
   `com.microsoft.teams*` → Teams, `us.zoom.*` → Zoom, configured browser ids (prefix match, so
   `com.google.Chrome.helper` counts as Chrome) → Google Meet. Core Audio reports the *helper* process that
   captures audio, not the app bundle, so all matching is by prefix.
   Zoom is additionally detected by the `CptHost` process (all platforms).
2. A signal must persist `start_confirm_secs` (default 5 s) before the meeting is confirmed.
3. On confirmation: emit `meeting-detector://meeting-detected`, notify, and if enabled start recording with the
   name `<App> YYYY-MM-DD HH-mm` through the same Rust path the tray uses. A recording that was already running
   is left alone and not owned by the detector.
4. When the signal disappears for `stop_grace_secs` (default 45 s) the meeting ends. Only a recording the detector
   started is stopped; the stop mirrors the tray (`stop_recording` + `recording-stop-complete`), so the webview
   saves transcripts to SQLite from whichever page it is on.

The transition itself is the pure function `detector::step`, so it is unit-tested with a fake clock; `run_monitor`
only performs the side effects for the `Action` it returns.

Safety rules the detector follows:

- Before auto-stopping, the recorder's current meeting name must still equal the one the detector started
  (`get_recording_meeting_name`), so a recording the user started in the meantime is never stopped.
- A probe that fails (Core Audio unavailable) is not read as "no meeting": the state machine is skipped that round,
  so a transient fault cannot run out the stop grace and end a recording. The failure is reported in
  `probe_error` and shown in the settings tab; it is logged only once.
- Turning the master switch off keeps an owned meeting in the status so that turning it back on during the same
  call resumes ownership and still auto-stops.
- A failed auto-start is not retried; the meeting stays confirmed but unowned, and the error is shown in the tab.

## Updater

The updater endpoint points at this fork's releases, so a fork build is never replaced by an upstream release.
Signed update artifacts are **not** produced (`bundle.createUpdaterArtifacts: false`): the public key in
`tauri.conf.json` is upstream's, the matching private key is not available here, and the build fails at the signing
step while producing an otherwise complete `.app` and `.dmg`. With no releases published in this repository the
update check simply fails and is logged; the startup check swallows the error and the tray item reports no update.

To turn updates back on: generate a key pair (`pnpm exec tauri signer generate -w ~/.tauri/meetily-fork.key`),
replace `plugins.updater.pubkey`, set `createUpdaterArtifacts` back to true, and publish releases carrying
`latest.json` with `TAURI_SIGNING_PRIVATE_KEY` set during the build.

## Known limits

- Microphone-based detection is macOS only (Core Audio process properties). Other platforms only get Zoom via `CptHost`.
- Google Meet is inferred from browser microphone use; any web call in a listed browser triggers it.
- Bundle identifier is still `com.meetily.ai`, so dev builds share `~/Library/Application Support/com.meetily.ai`
  with an installed upstream build. Back up `meeting_minutes.sqlite` before running a dev build alongside it.

- The transcription language is a webview `localStorage` value (`primaryLanguage`) synced into a Rust static at
  startup. The dev build (origin `http://localhost:3118`) has its own storage, so set the language again in
  Settings → Transcription when testing with `tauri dev`; a recording auto-started before the webview finished
  syncing falls back to the default (`auto`).

## Verify

```bash
cd frontend/src-tauri && cargo test --lib meeting_detector   # 24 unit tests (state machine, settings, signals)
cd frontend/src-tauri && cargo test --lib meeting_detector -- --ignored --nocapture  # live Core Audio probe
cd frontend && pnpm exec tsc --noEmit                        # frontend types
cd frontend && bun test tests/components tests/lib           # frontend tests (bun, no npm script)
cd frontend && ./clean_run.sh                                # run; Settings → Auto Record → Check now during a call
```

Running the dev build requires the installed `/Applications/meetily.app` to be quit first: both share the bundle id
`com.meetily.ai`, so the single-instance guard makes the dev build hand focus to the installed app and exit.
