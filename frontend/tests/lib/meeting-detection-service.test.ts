import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';

// Bun shares module mocks between test files; restore application modules after this suite.
const originalCore = { ...(await import('@tauri-apps/api/core')) };
const originalEvent = { ...(await import('@tauri-apps/api/event')) };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
});

let invoked: Array<{ command: string; args?: Record<string, unknown> }> = [];
const invoke = mock(async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
  invoked.push({ command, args });
  return command === 'plugin:meeting-detector|probe_signals' ? [] : {};
});
mock.module('@tauri-apps/api/core', () => ({ invoke }));

let listened: Array<{ event: string; handler: (e: { payload: unknown }) => void }> = [];
const unlisten = mock(() => {});
const listen = mock(async (event: string, handler: (e: { payload: unknown }) => void) => {
  listened.push({ event, handler });
  return unlisten;
});
mock.module('@tauri-apps/api/event', () => ({ listen }));

const { MEETING_APP_LABELS, appLabel, meetingDetectionService } = await import(
  '../../src/components/MeetingDetection/meetingDetectionService'
);

const settings = {
  enabled: true,
  auto_start_recording: true,
  auto_stop_recording: true,
  detect_teams: true,
  detect_google_meet: true,
  detect_zoom: true,
  notify_on_detection: true,
  poll_interval_secs: 3,
  start_confirm_secs: 5,
  stop_grace_secs: 45,
  meet_browser_bundle_ids: ['com.google.Chrome'],
};

describe('meetingDetectionService', () => {
  beforeEach(() => {
    invoked = [];
    listened = [];
  });

  test('commands are namespaced to the meeting-detector plugin', async () => {
    await meetingDetectionService.getSettings();
    await meetingDetectionService.getStatus();
    await meetingDetectionService.probeSignals();
    expect(invoked.map((i) => i.command)).toEqual([
      'plugin:meeting-detector|get_settings',
      'plugin:meeting-detector|get_status',
      'plugin:meeting-detector|probe_signals',
    ]);
  });

  test('setSettings passes the settings object under a settings key', async () => {
    await meetingDetectionService.setSettings(settings);
    expect(invoked).toEqual([
      { command: 'plugin:meeting-detector|set_settings', args: { settings } },
    ]);
  });

  test('onStatus subscribes to the status event and forwards the payload', async () => {
    const seen: unknown[] = [];
    const off = await meetingDetectionService.onStatus((st) => seen.push(st));

    expect(listened).toHaveLength(1);
    expect(listened[0].event).toBe('meeting-detector://status');

    listened[0].handler({ payload: { monitoring: true } });
    expect(seen).toEqual([{ monitoring: true }]);

    expect(off).toBe(unlisten);
  });

  test('every MeetingApp variant has a label', () => {
    // Mirrors the Rust enum in meeting_detector/signals.rs; a new variant must be added here too.
    expect(Object.keys(MEETING_APP_LABELS).sort()).toEqual(['GoogleMeet', 'MicrosoftTeams', 'Zoom']);
    expect(appLabel('MicrosoftTeams')).toBe('Microsoft Teams');
  });

  test('appLabel falls back to the raw value for an unknown variant', () => {
    expect(appLabel('WebexFromTheFuture')).toBe('WebexFromTheFuture');
  });
});
