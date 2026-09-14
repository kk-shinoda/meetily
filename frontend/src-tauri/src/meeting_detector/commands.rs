//! Tauri commands exposed by the `meeting-detector` inlined plugin.
//! Invoke from the webview as `invoke('plugin:meeting-detector|<name>')`.

use super::detector::{DetectionStatus, DetectorState};
use super::settings::MeetingDetectionSettings;
use super::signals::{probe_all, ProcessProbe, Signal};
use log::debug;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};

#[tauri::command]
pub async fn get_settings(state: State<'_, Arc<DetectorState>>) -> Result<MeetingDetectionSettings, String> {
    Ok(state.settings.read().await.clone())
}

#[tauri::command]
pub async fn set_settings<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, Arc<DetectorState>>,
    settings: MeetingDetectionSettings,
) -> Result<MeetingDetectionSettings, String> {
    let settings = settings.sanitized();
    debug!("meeting_detector: settings updated: {:?}", settings);
    settings.save(&app)?;
    {
        let mut current = state.settings.write().await;
        *current = settings.clone();
    }
    state.wake.notify_one();
    Ok(settings)
}

#[tauri::command]
pub async fn get_status(state: State<'_, Arc<DetectorState>>) -> Result<DetectionStatus, String> {
    Ok(state.status.read().await.clone())
}

/// One-shot probe ignoring the master switch and per-app toggles. Useful to
/// verify that the platform actually reports microphone use for the running
/// meeting app. A platform failure is returned as `Err`.
#[tauri::command]
pub async fn probe_signals(state: State<'_, Arc<DetectorState>>) -> Result<Vec<Signal>, String> {
    let mut settings = state.settings.read().await.clone();
    settings.detect_teams = true;
    settings.detect_google_meet = true;
    settings.detect_zoom = true;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut probe = ProcessProbe::default();
        probe_all(&mut probe, &settings)
    })
    .await
    .map_err(|e| format!("probe failed: {}", e))?;
    match outcome.error {
        Some(e) => Err(e),
        None => Ok(outcome.signals),
    }
}
