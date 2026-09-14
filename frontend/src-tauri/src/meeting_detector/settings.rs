//! Persisted settings for automatic meeting detection.
//!
//! Stored as `meeting_detection.json` in the app data directory so that the
//! Rust side can read them at startup without waiting for the webview.

use log::{error, info};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};

pub const SETTINGS_FILE_NAME: &str = "meeting_detection.json";

/// Browser bundle identifiers (macOS) whose microphone use is treated as a
/// Google Meet call. Matched as prefixes (`com.google.Chrome` also matches the
/// `com.google.Chrome.helper` process that actually captures audio). Editable
/// from the settings UI.
pub const DEFAULT_MEET_BROWSER_BUNDLE_IDS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "company.thebrowser.Browser", // Arc
    "org.chromium.Chromium",
    "org.mozilla.firefox",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MeetingDetectionSettings {
    /// Master switch. Off by default (opt-in).
    pub enabled: bool,
    /// Start recording automatically when a meeting is confirmed.
    pub auto_start_recording: bool,
    /// Stop recording automatically when the meeting signal has been gone for
    /// `stop_grace_secs`. Only recordings started by the detector are stopped.
    pub auto_stop_recording: bool,
    pub detect_teams: bool,
    pub detect_google_meet: bool,
    pub detect_zoom: bool,
    /// Show an OS notification when a meeting is detected / ended.
    pub notify_on_detection: bool,
    /// Seconds between probes.
    pub poll_interval_secs: u64,
    /// A signal must persist this long before a meeting is confirmed
    /// (filters out short microphone permission checks).
    pub start_confirm_secs: u64,
    /// A confirmed meeting ends only after its signal has been absent this long
    /// (hysteresis against network blips / brief mute-release).
    pub stop_grace_secs: u64,
    /// Browsers whose microphone use counts as Google Meet (macOS bundle ids).
    pub meet_browser_bundle_ids: Vec<String>,
}

impl Default for MeetingDetectionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_start_recording: true,
            auto_stop_recording: true,
            detect_teams: true,
            detect_google_meet: true,
            detect_zoom: true,
            notify_on_detection: true,
            poll_interval_secs: 3,
            start_confirm_secs: 5,
            stop_grace_secs: 45,
            meet_browser_bundle_ids: DEFAULT_MEET_BROWSER_BUNDLE_IDS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl MeetingDetectionSettings {
    /// Clamp user-provided values into a safe range.
    pub fn sanitized(mut self) -> Self {
        self.poll_interval_secs = self.poll_interval_secs.clamp(1, 60);
        self.start_confirm_secs = self.start_confirm_secs.clamp(0, 120);
        self.stop_grace_secs = self.stop_grace_secs.clamp(0, 600);
        let mut ids: Vec<String> = Vec::new();
        for id in self.meet_browser_bundle_ids {
            let id = id.trim().to_string();
            if !id.is_empty() && !ids.iter().any(|x| x.eq_ignore_ascii_case(&id)) {
                ids.push(id);
            }
        }
        self.meet_browser_bundle_ids = ids;
        self
    }

    pub fn settings_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
        match app.path().app_data_dir() {
            Ok(dir) => Some(dir.join(SETTINGS_FILE_NAME)),
            Err(e) => {
                error!("meeting_detector: failed to resolve app data dir: {}", e);
                None
            }
        }
    }

    pub fn load<R: Runtime>(app: &AppHandle<R>) -> Self {
        let Some(path) = Self::settings_path(app) else {
            return Self::default();
        };
        if !path.exists() {
            info!("meeting_detector: no settings file at {:?}, using defaults", path);
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Self>(&contents) {
                Ok(settings) => {
                    info!("meeting_detector: loaded settings from {:?}", path);
                    settings.sanitized()
                }
                Err(e) => {
                    error!("meeting_detector: failed to parse {:?}: {}", path, e);
                    Self::default()
                }
            },
            Err(e) => {
                error!("meeting_detector: failed to read {:?}: {}", path, e);
                Self::default()
            }
        }
    }

    pub fn save<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(), String> {
        let path = Self::settings_path(app).ok_or("Could not determine settings path")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings directory: {}", e))?;
        }
        let contents = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;
        std::fs::write(&path, contents).map_err(|e| format!("Failed to write settings: {}", e))?;
        info!("meeting_detector: saved settings to {:?}", path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_with_missing_fields_using_defaults() {
        let s: MeetingDetectionSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        let d = MeetingDetectionSettings::default();
        assert!(s.enabled);
        assert_eq!(s.auto_start_recording, d.auto_start_recording);
        assert_eq!(s.stop_grace_secs, d.stop_grace_secs);
        assert_eq!(s.meet_browser_bundle_ids, d.meet_browser_bundle_ids);
    }

    #[test]
    fn ignores_unknown_fields() {
        let s: MeetingDetectionSettings =
            serde_json::from_str(r#"{"enabled":true,"future_field":1}"#).unwrap();
        assert!(s.enabled);
    }

    #[test]
    fn sanitized_clamps_trims_and_dedupes() {
        let s = MeetingDetectionSettings {
            poll_interval_secs: 0,
            start_confirm_secs: 999,
            stop_grace_secs: 10_000,
            meet_browser_bundle_ids: vec!["  a ".into(), "".into(), "b".into(), "A".into()],
            ..Default::default()
        }
        .sanitized();
        assert_eq!(s.poll_interval_secs, 1);
        assert_eq!(s.start_confirm_secs, 120);
        assert_eq!(s.stop_grace_secs, 600);
        assert_eq!(s.meet_browser_bundle_ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn roundtrip_json() {
        let d = MeetingDetectionSettings::default();
        let json = serde_json::to_string_pretty(&d).unwrap();
        let back: MeetingDetectionSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
