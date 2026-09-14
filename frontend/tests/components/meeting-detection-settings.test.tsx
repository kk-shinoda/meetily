import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import type { DetectionStatus, MeetingDetectionSettings as Settings } from '../../src/components/MeetingDetection/meetingDetectionService';

// Bun shares module mocks between test files; restore application modules after this suite.
const originalCore = { ...(await import('@tauri-apps/api/core')) };
const originalEvent = { ...(await import('@tauri-apps/api/event')) };
const originalToast = { ...(await import('sonner')) };
const originalSwitch = { ...(await import('../../src/components/ui/switch')) };
afterAll(() => {
  mock.module('@tauri-apps/api/core', () => originalCore);
  mock.module('@tauri-apps/api/event', () => originalEvent);
  mock.module('sonner', () => originalToast);
  mock.module('../../src/components/ui/switch', () => originalSwitch);
});

const toastError = mock(() => {});
mock.module('sonner', () => ({ toast: { error: toastError, info: toastError, success: toastError, warning: toastError } }));

// Radix Switch measures the DOM; react-test-renderer has none.
mock.module('../../src/components/ui/switch', () => ({
  Switch: ({ checked, onCheckedChange, disabled, ...rest }: any) => (
    <button role="switch" aria-checked={checked} disabled={disabled} onClick={() => onCheckedChange(!checked)} {...rest} />
  ),
}));
// Radix Label pulls in DOM-dependent primitives; a plain label is enough here.
mock.module('../../src/components/ui/label', () => ({
  Label: ({ children, ...rest }: any) => <label {...rest}>{children}</label>,
}));

const baseSettings: Settings = {
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
  meet_browser_bundle_ids: ['com.google.Chrome'],
};

const baseStatus: DetectionStatus = {
  monitoring: true,
  current_meeting: null,
  pending_app: null,
  pending_secs: null,
  signal_lost_secs: null,
  last_signals: [],
  last_probe_at: null,
  probe_error: null,
  last_error: null,
};

let stored: Settings;
let saves: Settings[];
let getSettingsImpl: () => Promise<Settings>;
let setSettingsImpl: (s: Settings) => Promise<Settings>;

const invoke = mock(async (command: string, args?: Record<string, unknown>): Promise<unknown> => {
  switch (command) {
    case 'plugin:meeting-detector|get_settings':
      return getSettingsImpl();
    case 'plugin:meeting-detector|get_status':
      return baseStatus;
    case 'plugin:meeting-detector|set_settings': {
      const next = args!.settings as Settings;
      saves.push(next);
      return setSettingsImpl(next);
    }
    case 'plugin:meeting-detector|probe_signals':
      return [];
    default:
      throw new Error(`Unexpected command: ${command}`);
  }
});
mock.module('@tauri-apps/api/core', () => ({ invoke }));

type StatusHandler = (event: { payload: DetectionStatus }) => void;
let statusHandler: StatusHandler | undefined;
let listenImpl: (handler: StatusHandler) => Promise<() => void>;
const listen = mock(async (_event: string, handler: StatusHandler) => listenImpl(handler));
mock.module('@tauri-apps/api/event', () => ({ listen }));

const { MeetingDetectionSettings } = await import('../../src/components/MeetingDetection/MeetingDetectionSettings');

const flush = async () => {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
};

async function render(): Promise<ReactTestRenderer> {
  let renderer!: ReactTestRenderer;
  await act(async () => {
    renderer = create(<MeetingDetectionSettings />);
  });
  await flush();
  return renderer;
}

/** Flattened visible text, so assertions do not depend on how React splits text nodes. */
function text(renderer: ReactTestRenderer): string {
  const walk = (node: unknown): string => {
    if (node == null || node === false) return '';
    if (typeof node === 'string' || typeof node === 'number') return String(node);
    if (Array.isArray(node)) return node.map(walk).join('');
    const children = (node as { children?: unknown }).children;
    return children ? walk(children) : '';
  };
  return walk(renderer.toJSON());
}
/** The host element (button/input/textarea) carrying this id, not the component wrapping it. */
const byId = (renderer: ReactTestRenderer, id: string) =>
  renderer.root.findAll((n) => typeof n.type === 'string' && n.props.id === id)[0];

describe('MeetingDetectionSettings', () => {
  beforeEach(() => {
    stored = { ...baseSettings, meet_browser_bundle_ids: [...baseSettings.meet_browser_bundle_ids] };
    saves = [];
    statusHandler = undefined;
    getSettingsImpl = async () => stored;
    setSettingsImpl = async (s) => {
      stored = s;
      return s;
    };
    listenImpl = async (handler) => {
      statusHandler = handler;
      return () => {};
    };
    toastError.mockClear();
  });

  test('loads settings and status on mount', async () => {
    const renderer = await render();
    expect(byId(renderer, 'md-enabled').props['aria-checked']).toBe(false);
    expect(byId(renderer, 'md-stop_grace_secs').props.value).toBe('45');
    expect(text(renderer)).toContain('Detection is off.');
  });

  test('toggling a switch saves the whole settings object and adopts the sanitized response', async () => {
    // The backend clamps; the UI must show what was actually stored.
    setSettingsImpl = async (s) => {
      stored = { ...s, poll_interval_secs: 60 };
      return stored;
    };
    const renderer = await render();

    await act(async () => {
      byId(renderer, 'md-enabled').props.onClick();
    });
    await flush();

    expect(saves).toHaveLength(1);
    expect(saves[0]).toEqual({ ...baseSettings, enabled: true });
    expect(byId(renderer, 'md-enabled').props['aria-checked']).toBe(true);
    expect(byId(renderer, 'md-poll_interval_secs').props.value).toBe('60');
  });

  test('a number field that is edited back to its stored value is not saved', async () => {
    const renderer = await render();
    const input = () => byId(renderer, 'md-stop_grace_secs');

    await act(async () => {
      input().props.onChange({ target: { value: '4' } });
    });
    await act(async () => {
      input().props.onChange({ target: { value: '45' } });
    });
    await act(async () => {
      input().props.onBlur();
    });
    await flush();

    expect(saves).toEqual([]);
  });

  test('blurring a number field clamps the value before saving', async () => {
    const renderer = await render();

    await act(async () => {
      byId(renderer, 'md-stop_grace_secs').props.onChange({ target: { value: '9999' } });
    });
    await act(async () => {
      byId(renderer, 'md-stop_grace_secs').props.onBlur();
    });
    await flush();

    expect(saves).toHaveLength(1);
    expect(saves[0].stop_grace_secs).toBe(600);
  });

  test('clearing a number field keeps the draft empty and saves nothing', async () => {
    const renderer = await render();

    await act(async () => {
      byId(renderer, 'md-poll_interval_secs').props.onChange({ target: { value: '' } });
    });
    expect(byId(renderer, 'md-poll_interval_secs').props.value).toBe('');

    await act(async () => {
      byId(renderer, 'md-poll_interval_secs').props.onBlur();
    });
    await flush();

    expect(saves).toEqual([]);
    expect(byId(renderer, 'md-poll_interval_secs').props.value).toBe('3');
  });

  test('the bundle id field splits on newlines and commas, trims, and drops empties', async () => {
    const renderer = await render();

    await act(async () => {
      byId(renderer, 'md-bundle-ids').props.onChange({ target: { value: ' com.google.Chrome \n\n org.mozilla.firefox, com.brave.Browser ' } });
    });
    await act(async () => {
      byId(renderer, 'md-bundle-ids').props.onBlur();
    });
    await flush();

    expect(saves).toHaveLength(1);
    expect(saves[0].meet_browser_bundle_ids).toEqual([
      'com.google.Chrome',
      'org.mozilla.firefox',
      'com.brave.Browser',
    ]);
  });

  test('an unchanged bundle id field is not saved', async () => {
    const renderer = await render();

    await act(async () => {
      byId(renderer, 'md-bundle-ids').props.onChange({ target: { value: 'com.google.Chrome\n' } });
    });
    await act(async () => {
      byId(renderer, 'md-bundle-ids').props.onBlur();
    });
    await flush();

    expect(saves).toEqual([]);
  });

  test('status events drive the status card', async () => {
    stored = { ...baseSettings, enabled: true };
    const renderer = await render();

    await act(async () => {
      statusHandler!({
        payload: { ...baseStatus, pending_app: 'Zoom', pending_secs: 2 },
      });
    });
    expect(text(renderer)).toContain('Zoom signal seen (2s)');

    await act(async () => {
      statusHandler!({
        payload: {
          ...baseStatus,
          current_meeting: {
            app: 'Zoom',
            display_name: 'Zoom',
            detected_at: '2026-09-12T10:14:44+09:00',
            meeting_name: 'Zoom 2026-09-12 10-14',
            recording_started_by_detector: true,
          },
          last_signals: [{ app: 'Zoom', source: 'MicrophoneInUse', detail: 'us.zoom.xos' }],
        },
      });
    });
    const rendered = text(renderer);
    expect(rendered).toContain('In meeting: Zoom');
    expect(rendered).toContain('Zoom 2026-09-12 10-14');
    expect(rendered).toContain('us.zoom.xos');
  });

  test('a probe error is surfaced in the status card', async () => {
    stored = { ...baseSettings, enabled: true };
    const renderer = await render();

    await act(async () => {
      statusHandler!({ payload: { ...baseStatus, probe_error: 'Core Audio process list unavailable' } });
    });

    expect(text(renderer)).toContain('Core Audio process list unavailable');
  });

  test('a failed load renders the unavailable panel and toasts once', async () => {
    getSettingsImpl = async () => {
      throw new Error('plugin not registered');
    };
    const renderer = await render();

    expect(text(renderer)).toContain('Meeting detection is unavailable');
    expect(text(renderer)).toContain('plugin not registered');
    expect(toastError).toHaveBeenCalledTimes(1);
  });

  test('unmounting before the listener resolves still disposes it', async () => {
    let resolveListen!: (fn: () => void) => void;
    listenImpl = () => new Promise((resolve) => { resolveListen = resolve; });

    let renderer!: ReactTestRenderer;
    await act(async () => {
      renderer = create(<MeetingDetectionSettings />);
    });
    await act(async () => {
      renderer.unmount();
    });

    const dispose = mock(() => {});
    await act(async () => {
      resolveListen(dispose);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(dispose).toHaveBeenCalledTimes(1);
  });
});
