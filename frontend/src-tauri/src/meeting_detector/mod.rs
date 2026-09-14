//! Automatic meeting detection (Google Meet / Microsoft Teams / Zoom).
//!
//! Packaged as a Tauri *inlined plugin* so the integration footprint in
//! `lib.rs` is two lines (`pub mod meeting_detector;` and
//! `.plugin(meeting_detector::init())`). Commands are declared for the ACL in
//! `build.rs` and allowed through the `meeting-detector:default` permission in
//! `tauri.conf.json`.
//!
//! Frontend entry point: `src/components/MeetingDetection/`.

pub mod commands;
pub mod detector;
pub mod settings;
pub mod signals;

use std::sync::Arc;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

pub const PLUGIN_NAME: &str = "meeting-detector";

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::<R>::new(PLUGIN_NAME)
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::set_settings,
            commands::get_status,
            commands::probe_signals,
        ])
        .setup(|app, _api| {
            let settings = settings::MeetingDetectionSettings::load(app);
            log::info!(
                "meeting_detector: initialized (enabled={}, auto_start={}, auto_stop={}, grace={}s)",
                settings.enabled,
                settings.auto_start_recording,
                settings.auto_stop_recording,
                settings.stop_grace_secs
            );
            let state = Arc::new(detector::DetectorState::new(settings));
            app.manage(state.clone());
            detector::spawn_monitor(app.clone(), state);
            Ok(())
        })
        .build()
}
