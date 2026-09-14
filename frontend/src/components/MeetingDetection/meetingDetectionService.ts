/**
 * Thin wrapper around the `meeting-detector` inlined Tauri plugin
 * (Rust: frontend/src-tauri/src/meeting_detector/).
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

export type MeetingApp = 'MicrosoftTeams' | 'Zoom' | 'GoogleMeet';
export type SignalSource = 'MicrophoneInUse' | 'ProcessRunning';

export interface MeetingDetectionSettings {
  enabled: boolean;
  auto_start_recording: boolean;
  auto_stop_recording: boolean;
  detect_teams: boolean;
  detect_google_meet: boolean;
  detect_zoom: boolean;
  notify_on_detection: boolean;
  poll_interval_secs: number;
  start_confirm_secs: number;
  stop_grace_secs: number;
  meet_browser_bundle_ids: string[];
}

export interface Signal {
  app: MeetingApp;
  source: SignalSource;
  detail: string;
}

export interface CurrentMeeting {
  app: MeetingApp;
  display_name: string;
  detected_at: string;
  meeting_name: string | null;
  recording_started_by_detector: boolean;
}

export interface DetectionStatus {
  monitoring: boolean;
  current_meeting: CurrentMeeting | null;
  pending_app: MeetingApp | null;
  pending_secs: number | null;
  signal_lost_secs: number | null;
  last_signals: Signal[];
  last_probe_at: string | null;
  /** Set while the platform probe itself is failing (e.g. unsupported macOS). */
  probe_error: string | null;
  /** Set when the last automatic start or stop failed. */
  last_error: string | null;
}

export const MEETING_APP_LABELS: Record<MeetingApp, string> = {
  MicrosoftTeams: 'Microsoft Teams',
  Zoom: 'Zoom',
  GoogleMeet: 'Google Meet',
};

/** Display name for an app, tolerating a variant this build does not know. */
export const appLabel = (app: MeetingApp | string): string =>
  MEETING_APP_LABELS[app as MeetingApp] ?? String(app);

const cmd = (name: string) => `plugin:meeting-detector|${name}`;

export const meetingDetectionService = {
  getSettings: () => invoke<MeetingDetectionSettings>(cmd('get_settings')),
  setSettings: (settings: MeetingDetectionSettings) =>
    invoke<MeetingDetectionSettings>(cmd('set_settings'), { settings }),
  getStatus: () => invoke<DetectionStatus>(cmd('get_status')),
  probeSignals: () => invoke<Signal[]>(cmd('probe_signals')),
  onStatus: (callback: (status: DetectionStatus) => void): Promise<UnlistenFn> =>
    listen<DetectionStatus>('meeting-detector://status', (event) => callback(event.payload)),
};
