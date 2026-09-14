//! Signal probes: "which meeting app looks like it is in a call right now?"
//!
//! Two independent sources are combined:
//! - macOS Core Audio: processes currently *capturing microphone input*
//!   (`kAudioProcessPropertyIsRunningInput`), mapped by bundle id.
//!   This is the primary signal for Teams and browser-based Google Meet.
//! - Process list (all platforms via `sysinfo`): Zoom spawns `CptHost`
//!   only while a meeting window is open.

use super::settings::MeetingDetectionSettings;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeetingApp {
    MicrosoftTeams,
    Zoom,
    GoogleMeet,
}

impl MeetingApp {
    pub fn display_name(&self) -> &'static str {
        match self {
            MeetingApp::MicrosoftTeams => "Microsoft Teams",
            MeetingApp::Zoom => "Zoom",
            MeetingApp::GoogleMeet => "Google Meet",
        }
    }

    /// Detection priority: dedicated apps first, browser heuristic last.
    pub const PRIORITY: [MeetingApp; 3] = [
        MeetingApp::MicrosoftTeams,
        MeetingApp::Zoom,
        MeetingApp::GoogleMeet,
    ];

    pub fn is_enabled(&self, settings: &MeetingDetectionSettings) -> bool {
        match self {
            MeetingApp::MicrosoftTeams => settings.detect_teams,
            MeetingApp::Zoom => settings.detect_zoom,
            MeetingApp::GoogleMeet => settings.detect_google_meet,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalSource {
    /// The process is capturing microphone input (macOS Core Audio).
    MicrophoneInUse,
    /// A meeting-only helper process exists (e.g. Zoom `CptHost`).
    ProcessRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub app: MeetingApp,
    pub source: SignalSource,
    /// Bundle id or process name that produced the signal.
    pub detail: String,
}

/// Process-list probe. Keeps a `sysinfo::System` alive between calls.
pub struct ProcessProbe {
    system: sysinfo::System,
}

impl Default for ProcessProbe {
    fn default() -> Self {
        Self {
            system: sysinfo::System::new(),
        }
    }
}

impl ProcessProbe {
    fn probe(&mut self, settings: &MeetingDetectionSettings, out: &mut Vec<Signal>) {
        if !settings.detect_zoom {
            return;
        }
        // Only the process list is needed; skip CPU/memory/disk stats.
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::new(),
        );
        for process in self.system.processes().values() {
            let name = process.name().to_string_lossy();
            if name.to_ascii_lowercase().contains("cpthost") {
                push_unique(
                    out,
                    Signal {
                        app: MeetingApp::Zoom,
                        source: SignalSource::ProcessRunning,
                        detail: name.to_string(),
                    },
                );
                break;
            }
        }
    }
}

fn push_unique(out: &mut Vec<Signal>, signal: Signal) {
    if !out.iter().any(|s| s.app == signal.app && s.source == signal.source) {
        out.push(signal);
    }
}

/// Map a macOS bundle identifier to a meeting app, honoring per-app toggles.
pub fn classify_bundle_id(bundle_id: &str, settings: &MeetingDetectionSettings) -> Option<MeetingApp> {
    if settings.detect_teams && bundle_id.starts_with("com.microsoft.teams") {
        return Some(MeetingApp::MicrosoftTeams);
    }
    if settings.detect_zoom && bundle_id.starts_with("us.zoom.") {
        return Some(MeetingApp::Zoom);
    }
    if settings.detect_google_meet
        && settings
            .meet_browser_bundle_ids
            .iter()
            .any(|id| bundle_id_matches(bundle_id, id))
    {
        return Some(MeetingApp::GoogleMeet);
    }
    None
}

/// Case-insensitive match of a Core Audio client bundle id against a configured
/// browser id. Browsers capture audio in helper processes, so
/// `com.google.Chrome.helper` must match the configured `com.google.Chrome`.
fn bundle_id_matches(bundle_id: &str, configured: &str) -> bool {
    let b = bundle_id.to_ascii_lowercase();
    let c = configured.to_ascii_lowercase();
    b == c || b.starts_with(&format!("{}.", c))
}

/// Result of one probe round. `error` is set when the platform query itself
/// failed (e.g. Core Audio process objects unavailable); `signals` is then
/// meaningless and callers must not treat it as "no meeting".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub signals: Vec<Signal>,
    pub error: Option<String>,
}

#[cfg(target_os = "macos")]
fn probe_microphone_users(settings: &MeetingDetectionSettings, out: &mut Vec<Signal>) -> Result<(), String> {
    use cidre::core_audio as ca;
    use std::sync::atomic::{AtomicBool, Ordering};

    static WARNED: AtomicBool = AtomicBool::new(false);

    let own_pid = std::process::id() as i32;
    let processes = match ca::System::processes() {
        Ok(p) => p,
        Err(e) => {
            // kAudioHardwarePropertyProcessObjectList needs a recent macOS
            // (14.x; exact floor unverified). Log once, report every time.
            if !WARNED.swap(true, Ordering::SeqCst) {
                log::warn!("meeting_detector: Core Audio process list unavailable ({:?}); microphone-based detection is off", e);
            }
            return Err(format!("Core Audio process list unavailable: {:?}", e));
        }
    };
    for process in processes {
        if !process.is_running_input().unwrap_or(false) {
            continue;
        }
        // Our own capture must never count as a meeting.
        if process.pid().map(|pid| pid == own_pid).unwrap_or(false) {
            continue;
        }
        let Ok(bundle_id) = process.bundle_id() else {
            continue;
        };
        let bundle_id = bundle_id.to_string();
        if let Some(app) = classify_bundle_id(&bundle_id, settings) {
            push_unique(
                out,
                Signal {
                    app,
                    source: SignalSource::MicrophoneInUse,
                    detail: bundle_id,
                },
            );
        } else {
            log::trace!("meeting_detector: mic in use by unrelated process {}", bundle_id);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn probe_microphone_users(_settings: &MeetingDetectionSettings, _out: &mut Vec<Signal>) -> Result<(), String> {
    // Microphone-usage detection is only implemented for macOS.
    // Teams / Google Meet detection therefore does not work on other platforms yet.
    Ok(())
}

/// Run every probe once. Signals are ordered by app priority and deduplicated
/// per (app, source).
pub fn probe_all(process_probe: &mut ProcessProbe, settings: &MeetingDetectionSettings) -> ProbeOutcome {
    let mut out = Vec::new();
    let error = probe_microphone_users(settings, &mut out).err();
    process_probe.probe(settings, &mut out);
    sort_by_priority(&mut out);
    ProbeOutcome { signals: out, error }
}

fn sort_by_priority(signals: &mut [Signal]) {
    signals.sort_by_key(|s| MeetingApp::PRIORITY.iter().position(|p| *p == s.app).unwrap_or(usize::MAX));
}

/// Pick the app to act on from a set of signals (highest priority first).
pub fn pick_app(signals: &[Signal]) -> Option<MeetingApp> {
    MeetingApp::PRIORITY
        .iter()
        .copied()
        .find(|app| signals.iter().any(|s| s.app == *app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_bundle_ids() {
        let s = MeetingDetectionSettings::default();
        assert_eq!(classify_bundle_id("com.microsoft.teams2", &s), Some(MeetingApp::MicrosoftTeams));
        assert_eq!(classify_bundle_id("us.zoom.xos", &s), Some(MeetingApp::Zoom));
        assert_eq!(classify_bundle_id("com.google.Chrome", &s), Some(MeetingApp::GoogleMeet));
        // Browsers and Teams capture audio in helper processes.
        assert_eq!(classify_bundle_id("com.google.Chrome.helper", &s), Some(MeetingApp::GoogleMeet));
        assert_eq!(classify_bundle_id("company.thebrowser.browser.helper", &s), Some(MeetingApp::GoogleMeet));
        assert_eq!(classify_bundle_id("com.microsoft.teams2.helper", &s), Some(MeetingApp::MicrosoftTeams));
        assert_eq!(classify_bundle_id("com.apple.FaceTime", &s), None);
        // No accidental prefix match on unrelated ids.
        assert_eq!(classify_bundle_id("com.google.Chromecast", &s), None);
    }

    #[test]
    fn toggles_disable_classification() {
        let s = MeetingDetectionSettings {
            detect_google_meet: false,
            ..Default::default()
        };
        assert_eq!(classify_bundle_id("com.google.Chrome", &s), None);
    }

    /// Manual check of the platform probe on this machine. Run with
    /// `cargo test --lib meeting_detector -- --ignored --nocapture` while a call is open.
    #[test]
    #[ignore]
    fn print_live_signals() {
        let settings = MeetingDetectionSettings::default();
        let mut probe = ProcessProbe::default();
        let outcome = probe_all(&mut probe, &settings);
        println!("live probe: {:#?}", outcome);
        #[cfg(target_os = "macos")]
        {
            use cidre::core_audio as ca;
            let procs = ca::System::processes().expect("Core Audio process list");
            println!("core audio client processes: {}", procs.len());
            for p in procs {
                println!(
                    "  pid={:?} bundle={:?} running={:?} input={:?} output={:?}",
                    p.pid().ok(),
                    p.bundle_id().ok().map(|b| b.to_string()),
                    p.is_running().ok(),
                    p.is_running_input().ok(),
                    p.is_running_output().ok()
                );
            }
        }
    }

    #[test]
    fn bundle_id_matches_is_case_insensitive_and_dot_bounded() {
        assert!(bundle_id_matches("COM.GOOGLE.CHROME.HELPER", "com.google.Chrome"));
        assert!(bundle_id_matches("com.google.chrome", "com.google.Chrome"));
        assert!(!bundle_id_matches("com.google.Chromecast", "com.google.Chrome"));
        assert!(!bundle_id_matches("com.google", "com.google.Chrome"));
    }

    #[test]
    fn push_unique_dedupes_per_app_and_source_and_sort_orders_by_priority() {
        let mut out = Vec::new();
        let mk = |app, source| Signal { app, source, detail: "x".into() };
        push_unique(&mut out, mk(MeetingApp::GoogleMeet, SignalSource::MicrophoneInUse));
        push_unique(&mut out, mk(MeetingApp::Zoom, SignalSource::ProcessRunning));
        push_unique(&mut out, mk(MeetingApp::Zoom, SignalSource::MicrophoneInUse));
        push_unique(&mut out, mk(MeetingApp::Zoom, SignalSource::MicrophoneInUse));
        sort_by_priority(&mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].app, MeetingApp::Zoom);
        assert_eq!(out[1].app, MeetingApp::Zoom);
        assert_eq!(out[2].app, MeetingApp::GoogleMeet);
    }

    #[test]
    fn pick_app_none_on_empty() {
        assert_eq!(pick_app(&[]), None);
    }

    #[test]
    fn process_probe_skipped_when_zoom_disabled() {
        let s = MeetingDetectionSettings { detect_zoom: false, ..Default::default() };
        let mut probe = ProcessProbe::default();
        let mut out = Vec::new();
        probe.probe(&s, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn pick_prefers_dedicated_apps() {
        let signals = vec![
            Signal { app: MeetingApp::GoogleMeet, source: SignalSource::MicrophoneInUse, detail: "chrome".into() },
            Signal { app: MeetingApp::Zoom, source: SignalSource::ProcessRunning, detail: "CptHost".into() },
        ];
        assert_eq!(pick_app(&signals), Some(MeetingApp::Zoom));
    }
}
