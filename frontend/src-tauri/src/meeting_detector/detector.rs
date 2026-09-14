//! Monitor loop: turns raw signals into confirmed meetings and drives the
//! recording lifecycle through the same Rust entry points the tray menu uses.
//!
//! State machine (per poll):
//!
//! ```text
//!   Idle ──signal──▶ Pending ──signal held ≥ start_confirm_secs──▶ Active
//!    ▲                 │ signal gone                                 │
//!    └─────────────────┘                                             │
//!    ▲                                                               ▼
//!    └────────── signal absent ≥ stop_grace_secs ◀── Active(lost_since=Some)
//! ```
//!
//! The pure transition lives in [`step`] so it can be unit-tested with a fake
//! clock; [`run_monitor`] performs the side effects for the returned
//! [`Action`]. Timers use `Instant`, which on macOS does not advance while the
//! machine sleeps, so a laptop that sleeps mid-grace resumes the grace timer
//! where it left off (it never ends a meeting early).

use super::settings::MeetingDetectionSettings;
use super::signals::{pick_app, probe_all, MeetingApp, ProbeOutcome, ProcessProbe, Signal};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{Notify, RwLock};

/// Event names emitted to the webview.
pub const EVENT_STATUS: &str = "meeting-detector://status";
pub const EVENT_MEETING_DETECTED: &str = "meeting-detector://meeting-detected";
pub const EVENT_MEETING_ENDED: &str = "meeting-detector://meeting-ended";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentMeeting {
    pub app: MeetingApp,
    pub display_name: String,
    /// RFC 3339 local time when the meeting was confirmed.
    pub detected_at: String,
    /// Meeting name passed to the recorder, if the detector started one.
    pub meeting_name: Option<String>,
    /// True when the detector started the recording (and therefore may stop it).
    pub recording_started_by_detector: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectionStatus {
    pub monitoring: bool,
    pub current_meeting: Option<CurrentMeeting>,
    /// Set while a signal is seen but not yet confirmed.
    pub pending_app: Option<MeetingApp>,
    /// Seconds the current signal has been held (pending) or absent (grace).
    pub pending_secs: Option<u64>,
    pub signal_lost_secs: Option<u64>,
    pub last_signals: Vec<Signal>,
    pub last_probe_at: Option<String>,
    /// Error from the most recent probe (cleared as soon as a probe succeeds).
    pub probe_error: Option<String>,
    /// Error from the last auto start/stop (cleared when the next one succeeds
    /// or a new meeting is confirmed).
    pub last_error: Option<String>,
}

/// Shared state managed by the Tauri plugin.
pub struct DetectorState {
    pub settings: RwLock<MeetingDetectionSettings>,
    pub status: RwLock<DetectionStatus>,
    /// Wakes the monitor loop immediately after a settings change.
    pub wake: Notify,
    started: AtomicBool,
}

impl DetectorState {
    pub fn new(settings: MeetingDetectionSettings) -> Self {
        Self {
            settings: RwLock::new(settings),
            status: RwLock::new(DetectionStatus::default()),
            wake: Notify::new(),
            started: AtomicBool::new(false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Pending { app: MeetingApp, since: Instant },
    Active { app: MeetingApp, lost_since: Option<Instant> },
}

/// Side effect requested by [`step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Start(MeetingApp),
    End(MeetingApp),
}

/// Pure state transition for one poll. `detected` is the highest-priority
/// enabled app among `signals` (see [`pick_app`]).
pub(crate) fn step(
    phase: Phase,
    signals: &[Signal],
    now: Instant,
    settings: &MeetingDetectionSettings,
) -> (Phase, Action) {
    let detected = pick_app(signals).filter(|a| a.is_enabled(settings));
    let present = |app: MeetingApp| signals.iter().any(|s| s.app == app);
    let confirm = Duration::from_secs(settings.start_confirm_secs);
    let grace = Duration::from_secs(settings.stop_grace_secs);

    match phase {
        Phase::Idle => match detected {
            Some(app) if confirm.is_zero() => (Phase::Active { app, lost_since: None }, Action::Start(app)),
            Some(app) => (Phase::Pending { app, since: now }, Action::None),
            None => (Phase::Idle, Action::None),
        },
        Phase::Pending { app, since } => {
            if !present(app) {
                (Phase::Idle, Action::None)
            } else if now.duration_since(since) >= confirm {
                (Phase::Active { app, lost_since: None }, Action::Start(app))
            } else {
                (Phase::Pending { app, since }, Action::None)
            }
        }
        Phase::Active { app, lost_since } => {
            if present(app) {
                (Phase::Active { app, lost_since: None }, Action::None)
            } else {
                match lost_since {
                    None if grace.is_zero() => (Phase::Idle, Action::End(app)),
                    None => (Phase::Active { app, lost_since: Some(now) }, Action::None),
                    Some(t) if now.duration_since(t) >= grace => (Phase::Idle, Action::End(app)),
                    Some(t) => (Phase::Active { app, lost_since: Some(t) }, Action::None),
                }
            }
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

/// Meeting name format required by the handoff: "<App> YYYY-MM-DD HH-mm".
pub fn build_meeting_name(app: MeetingApp) -> String {
    format!(
        "{} {}",
        app.display_name(),
        chrono::Local::now().format("%Y-%m-%d %H-%M")
    )
}

fn notify<R: Runtime>(app: &AppHandle<R>, settings: &MeetingDetectionSettings, body: String) {
    if !settings.notify_on_detection {
        return;
    }
    if let Err(e) = app
        .notification()
        .builder()
        .title("Meetily")
        .body(&body)
        .show()
    {
        warn!("meeting_detector: notification failed: {}", e);
    }
}

fn emit<R: Runtime, S: Serialize + Clone>(app: &AppHandle<R>, event: &str, payload: S) {
    if let Err(e) = app.emit(event, payload) {
        warn!("meeting_detector: failed to emit {}: {}", event, e);
    }
}

async fn publish_status<R: Runtime>(app: &AppHandle<R>, state: &DetectorState) {
    let snapshot = state.status.read().await.clone();
    emit(app, EVENT_STATUS, snapshot);
}

/// Spawn the monitor loop once. Safe to call multiple times.
///
/// Called from the plugin `setup`, which runs before the app's own `setup`
/// creates the tray. A meeting confirmed in that window (only possible with
/// `start_confirm_secs = 0`) would just log a tray warning.
pub fn spawn_monitor<R: Runtime>(app: AppHandle<R>, state: Arc<DetectorState>) {
    if state.started.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        run_monitor(app, state).await;
    });
}

async fn run_monitor<R: Runtime>(app: AppHandle<R>, state: Arc<DetectorState>) {
    info!("meeting_detector: monitor task started");
    let process_probe = Arc::new(std::sync::Mutex::new(ProcessProbe::default()));
    let mut phase = Phase::Idle;

    loop {
        let settings = state.settings.read().await.clone();

        if !settings.enabled {
            // Keep an owned meeting so that re-enabling during the same call
            // restores ownership and the recording is still auto-stopped.
            let owned = {
                let mut status = state.status.write().await;
                let owned = status
                    .current_meeting
                    .take()
                    .filter(|m| m.recording_started_by_detector);
                *status = DetectionStatus {
                    current_meeting: owned.clone(),
                    ..DetectionStatus::default()
                };
                owned
            };
            if owned.is_some() {
                info!("meeting_detector: disabled while an owned recording is running; it stays owned until re-enabled or stopped");
            }
            phase = Phase::Idle;
            publish_status(&app, &state).await;
            state.wake.notified().await;
            // Restore the active phase for a meeting we still own.
            if let Some(m) = state.status.read().await.current_meeting.as_ref() {
                if m.recording_started_by_detector {
                    phase = Phase::Active { app: m.app, lost_since: None };
                    info!("meeting_detector: re-enabled; resuming ownership of {}", m.display_name);
                }
            }
            continue;
        }

        // Probe on a blocking thread: sysinfo refresh and Core Audio queries are
        // synchronous and may take tens of milliseconds.
        let probe_settings = settings.clone();
        let probe_handle = process_probe.clone();
        let probe_result = tokio::task::spawn_blocking(move || {
            let mut guard = probe_handle.lock().unwrap_or_else(|p| p.into_inner());
            probe_all(&mut guard, &probe_settings)
        })
        .await;

        let now = Instant::now();
        let outcome: ProbeOutcome = match probe_result {
            Ok(o) => o,
            Err(e) => ProbeOutcome {
                signals: Vec::new(),
                error: Some(format!("probe task failed: {}", e)),
            },
        };

        // A failed probe says nothing about the meeting: do not advance the
        // state machine (in particular, do not start or extend the grace timer).
        let action = if outcome.error.is_none() {
            let (next, action) = step(phase, &outcome.signals, now, &settings);
            phase = next;
            action
        } else {
            Action::None
        };

        match action {
            Action::Start(kind) => on_meeting_started(&app, &state, &settings, kind, &outcome.signals).await,
            Action::End(kind) => on_meeting_ended(&app, &state, &settings, kind).await,
            Action::None => {}
        }

        {
            let mut status = state.status.write().await;
            status.monitoring = true;
            status.last_probe_at = Some(now_rfc3339());
            if outcome.error.is_none() {
                status.last_signals = outcome.signals;
            }
            status.probe_error = outcome.error;
            match &phase {
                Phase::Idle => {
                    status.pending_app = None;
                    status.pending_secs = None;
                    status.signal_lost_secs = None;
                }
                Phase::Pending { app: a, since } => {
                    status.pending_app = Some(*a);
                    status.pending_secs = Some(now.duration_since(*since).as_secs());
                    status.signal_lost_secs = None;
                }
                Phase::Active { lost_since, .. } => {
                    status.pending_app = None;
                    status.pending_secs = None;
                    status.signal_lost_secs = lost_since.map(|t| now.duration_since(t).as_secs());
                }
            }
        }
        publish_status(&app, &state).await;

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(settings.poll_interval_secs)) => {}
            _ = state.wake.notified() => {}
        }
    }
}

async fn on_meeting_started<R: Runtime>(
    app: &AppHandle<R>,
    state: &DetectorState,
    settings: &MeetingDetectionSettings,
    kind: MeetingApp,
    signals: &[Signal],
) {
    info!(
        "meeting_detector: meeting confirmed: {} (signals: {:?})",
        kind.display_name(),
        signals
    );

    let mut meeting = CurrentMeeting {
        app: kind,
        display_name: kind.display_name().to_string(),
        detected_at: now_rfc3339(),
        meeting_name: None,
        recording_started_by_detector: false,
    };

    emit(app, EVENT_MEETING_DETECTED, meeting.clone());

    let mut error: Option<String> = None;
    if settings.auto_start_recording {
        if crate::audio::recording_commands::is_recording().await {
            info!("meeting_detector: a recording is already running; not taking ownership of it");
            notify(app, settings, format!("{} meeting detected. Recording already in progress.", kind.display_name()));
        } else {
            let name = build_meeting_name(kind);
            info!("meeting_detector: auto-starting recording \"{}\"", name);
            // start_recording_with_meeting_name refreshes the tray itself.
            match crate::audio::recording_commands::start_recording_with_meeting_name(
                app.clone(),
                Some(name.clone()),
            )
            .await
            {
                Ok(()) => {
                    meeting.meeting_name = Some(name.clone());
                    meeting.recording_started_by_detector = true;
                    notify(app, settings, format!("Recording started: {}", name));
                }
                Err(e) => {
                    // No retry: a later poll only re-checks the signal, not the
                    // recorder. The meeting stays confirmed but unowned.
                    error!("meeting_detector: auto-start failed: {}", e);
                    error = Some(format!("auto-start failed: {}", e));
                    notify(app, settings, format!("{} meeting detected, but recording could not start: {}", kind.display_name(), e));
                }
            }
        }
    } else {
        notify(app, settings, format!("{} meeting detected.", kind.display_name()));
    }

    let mut status = state.status.write().await;
    status.current_meeting = Some(meeting);
    status.last_error = error;
}

async fn on_meeting_ended<R: Runtime>(
    app: &AppHandle<R>,
    state: &DetectorState,
    settings: &MeetingDetectionSettings,
    kind: MeetingApp,
) {
    info!("meeting_detector: meeting ended: {}", kind.display_name());

    let owned_name: Option<String> = state
        .status
        .read()
        .await
        .current_meeting
        .as_ref()
        .filter(|m| m.recording_started_by_detector)
        .and_then(|m| m.meeting_name.clone());

    emit(
        app,
        EVENT_MEETING_ENDED,
        serde_json::json!({ "app": kind, "display_name": kind.display_name() }),
    );

    let mut error: Option<String> = None;
    match owned_name {
        Some(name) if settings.auto_stop_recording => {
            if !crate::audio::recording_commands::is_recording().await {
                info!("meeting_detector: recording was already stopped manually");
            } else if !current_recording_is(&name).await {
                // The user stopped ours and started a different recording.
                info!("meeting_detector: a different recording is running; leaving it alone");
            } else {
                match stop_recording_like_tray(app).await {
                    Ok(()) => notify(app, settings, format!("{} meeting ended. Recording saved.", kind.display_name())),
                    Err(e) => {
                        error!("meeting_detector: auto-stop failed: {}", e);
                        error = Some(format!("auto-stop failed: {}", e));
                        notify(app, settings, format!("{} meeting ended, but recording could not be stopped: {}", kind.display_name(), e));
                    }
                }
            }
        }
        Some(_) => info!("meeting_detector: auto-stop disabled; leaving recording running"),
        None => info!("meeting_detector: recording not started by detector; leaving it running"),
    }

    let mut status = state.status.write().await;
    status.current_meeting = None;
    if error.is_some() {
        status.last_error = error;
    } else if status.last_error.as_deref().map_or(false, |e| e.starts_with("auto-stop")) {
        status.last_error = None;
    }
}

/// True when the recorder's current meeting name equals `name`.
async fn current_recording_is(name: &str) -> bool {
    match crate::audio::recording_commands::get_recording_meeting_name().await {
        Ok(Some(current)) => current == name,
        Ok(None) => false,
        Err(e) => {
            warn!("meeting_detector: could not read current meeting name: {}", e);
            false
        }
    }
}

/// Mirrors `tray::stop_recording_handler`: stop the native recorder, then let
/// the webview run its post-processing (SQLite save, navigation) from any page
/// via the `recording-stop-complete` event. `stop_recording` refreshes the
/// tray itself on success; on failure the tray is reverted like the tray does.
async fn stop_recording_like_tray<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let save_path = data_dir.join(format!("recording-{}.wav", timestamp));

    crate::tray::set_tray_state(app, crate::tray::RecordingState::Stopping);

    let result = crate::audio::recording_commands::stop_recording(
        app.clone(),
        crate::audio::recording_commands::RecordingArgs {
            save_path: save_path.to_string_lossy().to_string(),
        },
    )
    .await;

    match result {
        Ok(()) => {
            // The recorder is stopped either way; a failed emit only means the
            // webview was not told to save, which is worth logging, not failing.
            if let Err(e) = app.emit("recording-stop-complete", true) {
                error!("meeting_detector: failed to emit recording-stop-complete: {}", e);
            }
            Ok(())
        }
        Err(e) => {
            crate::tray::update_tray_menu(app);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::signals::SignalSource;

    fn settings(confirm: u64, grace: u64) -> MeetingDetectionSettings {
        MeetingDetectionSettings {
            start_confirm_secs: confirm,
            stop_grace_secs: grace,
            ..Default::default()
        }
    }

    fn sig(app: MeetingApp) -> Signal {
        Signal { app, source: SignalSource::MicrophoneInUse, detail: "test".into() }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn idle_to_pending_on_first_signal() {
        let now = Instant::now();
        let (phase, action) = step(Phase::Idle, &[sig(MeetingApp::Zoom)], now, &settings(5, 45));
        assert_eq!(phase, Phase::Pending { app: MeetingApp::Zoom, since: now });
        assert_eq!(action, Action::None);
    }

    #[test]
    fn idle_stays_idle_without_signal() {
        let (phase, action) = step(Phase::Idle, &[], Instant::now(), &settings(5, 45));
        assert_eq!(phase, Phase::Idle);
        assert_eq!(action, Action::None);
    }

    #[test]
    fn idle_to_active_immediately_when_confirm_is_zero() {
        let now = Instant::now();
        let (phase, action) = step(Phase::Idle, &[sig(MeetingApp::Zoom)], now, &settings(0, 45));
        assert_eq!(phase, Phase::Active { app: MeetingApp::Zoom, lost_since: None });
        assert_eq!(action, Action::Start(MeetingApp::Zoom));
    }

    #[test]
    fn idle_ignores_disabled_app() {
        let s = MeetingDetectionSettings { detect_zoom: false, ..settings(5, 45) };
        let (phase, _) = step(Phase::Idle, &[sig(MeetingApp::Zoom)], Instant::now(), &s);
        assert_eq!(phase, Phase::Idle);
    }

    #[test]
    fn pending_returns_to_idle_when_signal_vanishes() {
        let t0 = Instant::now();
        let pending = Phase::Pending { app: MeetingApp::GoogleMeet, since: t0 };
        let (phase, action) = step(pending, &[], t0 + secs(2), &settings(5, 45));
        assert_eq!(phase, Phase::Idle);
        assert_eq!(action, Action::None);
    }

    #[test]
    fn pending_ignores_other_app() {
        let t0 = Instant::now();
        let pending = Phase::Pending { app: MeetingApp::GoogleMeet, since: t0 };
        let (phase, _) = step(pending, &[sig(MeetingApp::Zoom)], t0 + secs(2), &settings(5, 45));
        assert_eq!(phase, Phase::Idle, "no switch mid-pending; the next poll starts over");
    }

    #[test]
    fn pending_confirms_after_confirm_window() {
        let t0 = Instant::now();
        let pending = Phase::Pending { app: MeetingApp::MicrosoftTeams, since: t0 };
        let s = settings(5, 45);

        let (phase, action) = step(pending, &[sig(MeetingApp::MicrosoftTeams)], t0 + secs(4), &s);
        assert_eq!(phase, pending);
        assert_eq!(action, Action::None);

        let (phase, action) = step(pending, &[sig(MeetingApp::MicrosoftTeams)], t0 + secs(5), &s);
        assert_eq!(phase, Phase::Active { app: MeetingApp::MicrosoftTeams, lost_since: None });
        assert_eq!(action, Action::Start(MeetingApp::MicrosoftTeams));
    }

    #[test]
    fn active_starts_grace_when_signal_lost() {
        let now = Instant::now();
        let active = Phase::Active { app: MeetingApp::Zoom, lost_since: None };
        let (phase, action) = step(active, &[], now, &settings(5, 45));
        assert_eq!(phase, Phase::Active { app: MeetingApp::Zoom, lost_since: Some(now) });
        assert_eq!(action, Action::None);
    }

    #[test]
    fn active_recovers_within_grace() {
        let t0 = Instant::now();
        let active = Phase::Active { app: MeetingApp::Zoom, lost_since: Some(t0) };
        let (phase, action) = step(active, &[sig(MeetingApp::Zoom)], t0 + secs(10), &settings(5, 45));
        assert_eq!(phase, Phase::Active { app: MeetingApp::Zoom, lost_since: None });
        assert_eq!(action, Action::None);
    }

    #[test]
    fn active_ends_after_grace() {
        let t0 = Instant::now();
        let active = Phase::Active { app: MeetingApp::Zoom, lost_since: Some(t0) };
        let s = settings(5, 45);

        let (phase, action) = step(active, &[], t0 + secs(44), &s);
        assert_eq!(phase, active);
        assert_eq!(action, Action::None);

        let (phase, action) = step(active, &[], t0 + secs(45), &s);
        assert_eq!(phase, Phase::Idle);
        assert_eq!(action, Action::End(MeetingApp::Zoom));
    }

    #[test]
    fn active_ignores_signals_from_other_apps_for_grace() {
        let now = Instant::now();
        let active = Phase::Active { app: MeetingApp::Zoom, lost_since: None };
        let (phase, _) = step(active, &[sig(MeetingApp::GoogleMeet)], now, &settings(5, 45));
        assert_eq!(phase, Phase::Active { app: MeetingApp::Zoom, lost_since: Some(now) });
    }

    #[test]
    fn stop_grace_zero_ends_on_first_absence() {
        let active = Phase::Active { app: MeetingApp::Zoom, lost_since: None };
        let (phase, action) = step(active, &[], Instant::now(), &settings(5, 0));
        assert_eq!(phase, Phase::Idle);
        assert_eq!(action, Action::End(MeetingApp::Zoom));
    }

    #[test]
    fn build_meeting_name_format() {
        let name = build_meeting_name(MeetingApp::GoogleMeet);
        let re = regex::Regex::new(r"^Google Meet \d{4}-\d{2}-\d{2} \d{2}-\d{2}$").unwrap();
        assert!(re.is_match(&name), "unexpected name: {}", name);
    }
}
