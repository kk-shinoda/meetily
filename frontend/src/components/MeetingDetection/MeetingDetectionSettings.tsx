'use client';

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Switch } from '@/components/ui/switch';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { AlertCircle, Radar, RefreshCw } from 'lucide-react';
import { toast } from 'sonner';
import {
  DetectionStatus,
  MeetingDetectionSettings as Settings,
  Signal,
  appLabel,
  meetingDetectionService,
} from './meetingDetectionService';

type NumberField = 'start_confirm_secs' | 'stop_grace_secs' | 'poll_interval_secs';

const NUMBER_FIELDS: Array<{ key: NumberField; label: string; help: string; min: number; max: number }> = [
  {
    key: 'start_confirm_secs',
    label: 'Start confirmation (seconds)',
    help: 'The meeting signal must persist this long before recording starts. Filters out short microphone checks.',
    min: 0,
    max: 120,
  },
  {
    key: 'stop_grace_secs',
    label: 'Stop grace period (seconds)',
    help: 'Recording stops only after the signal has been gone this long. Protects against brief network drops.',
    min: 0,
    max: 600,
  },
  {
    key: 'poll_interval_secs',
    label: 'Check interval (seconds)',
    help: 'How often running apps and microphone use are inspected.',
    min: 1,
    max: 60,
  },
];

const APP_TOGGLES: Array<{ key: 'detect_teams' | 'detect_zoom' | 'detect_google_meet'; label: string; help: string }> = [
  { key: 'detect_teams', label: 'Microsoft Teams', help: 'Teams desktop app capturing the microphone.' },
  { key: 'detect_zoom', label: 'Zoom', help: 'Zoom capturing the microphone, or the CptHost meeting process.' },
  {
    key: 'detect_google_meet',
    label: 'Google Meet',
    help: 'A listed browser capturing the microphone. Other calls in that browser are also detected.',
  },
];

const errorText = (error: unknown): string => (error instanceof Error ? error.message : String(error));

function formatTime(iso: string | null): string {
  if (!iso) return '-';
  const d = new Date(iso);
  return isNaN(d.getTime()) ? iso : d.toLocaleTimeString();
}

function SignalList({ signals }: { signals: Signal[] }) {
  if (signals.length === 0) {
    return <div className="text-sm text-gray-500">No meeting signals</div>;
  }
  return (
    <ul className="text-sm space-y-1">
      {signals.map((s) => (
        <li key={`${s.app}-${s.source}`} className="flex items-center gap-2">
          <span className="font-medium">{appLabel(s.app)}</span>
          <span className="text-gray-500">
            {s.source === 'MicrophoneInUse' ? 'microphone in use' : 'process running'}
          </span>
          <span className="text-xs text-gray-400 font-mono">{s.detail}</span>
        </li>
      ))}
    </ul>
  );
}

export function MeetingDetectionSettings() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [status, setStatus] = useState<DetectionStatus | null>(null);
  const [saving, setSaving] = useState(false);
  const [checking, setChecking] = useState(false);
  const [checkResult, setCheckResult] = useState<Signal[] | null>(null);
  // Number inputs keep a string draft so that clearing the field does not snap to 0.
  const [drafts, setDrafts] = useState<Partial<Record<NumberField, string>>>({});
  const [bundleIdsText, setBundleIdsText] = useState('');
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadAttempt, setLoadAttempt] = useState(0);
  // Only the newest save response may overwrite local state.
  const saveSeq = useRef(0);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    const adopt = (s: Settings) => {
      setSettings(s);
      setBundleIdsText(s.meet_browser_bundle_ids.join('\n'));
      setDrafts({});
    };

    (async () => {
      // Subscribe first so that a status emitted during the initial load is not missed.
      try {
        const fn = await meetingDetectionService.onStatus((st) => {
          if (!cancelled) setStatus(st);
        });
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      } catch (error) {
        console.error('Failed to subscribe to meeting detection status:', error);
      }

      try {
        const [s, st] = await Promise.all([
          meetingDetectionService.getSettings(),
          meetingDetectionService.getStatus(),
        ]);
        if (cancelled) return;
        adopt(s);
        setStatus(st);
        setLoadError(null);
      } catch (error) {
        console.error('Failed to load meeting detection settings:', error);
        if (cancelled) return;
        setLoadError(errorText(error));
        toast.error('Failed to load meeting detection settings', { description: errorText(error) });
      }
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadAttempt]);

  const persist = useCallback(async (next: Settings) => {
    const seq = ++saveSeq.current;
    setSaving(true);
    try {
      const saved = await meetingDetectionService.setSettings(next);
      // A later save already answered; its state wins.
      if (seq !== saveSeq.current) return;
      setSettings(saved);
      setBundleIdsText(saved.meet_browser_bundle_ids.join('\n'));
      setDrafts({});
    } catch (error) {
      console.error('Failed to save meeting detection settings:', error);
      toast.error('Failed to save settings', { description: errorText(error) });
    } finally {
      if (seq === saveSeq.current) setSaving(false);
    }
  }, []);

  const update = useCallback(
    (patch: Partial<Settings>) => {
      setSettings((current) => {
        if (!current) return current;
        const next = { ...current, ...patch };
        void persist(next);
        return next;
      });
    },
    [persist]
  );

  const setField = useCallback(
    <K extends keyof Settings>(key: K, value: Settings[K]) => update({ [key]: value } as Pick<Settings, K>),
    [update]
  );

  const commitBundleIds = useCallback(() => {
    if (!settings) return;
    const ids = bundleIdsText
      .split(/\r?\n|,/)
      .map((s) => s.trim())
      .filter(Boolean);
    if (JSON.stringify(ids) !== JSON.stringify(settings.meet_browser_bundle_ids)) {
      setField('meet_browser_bundle_ids', ids);
    } else {
      setBundleIdsText(settings.meet_browser_bundle_ids.join('\n'));
    }
  }, [bundleIdsText, settings, setField]);

  const commitNumber = useCallback(
    (field: (typeof NUMBER_FIELDS)[number]) => {
      if (!settings) return;
      const draft = drafts[field.key];
      setDrafts((d) => {
        const { [field.key]: _dropped, ...rest } = d;
        return rest;
      });
      if (draft === undefined) return;
      const parsed = Number(draft);
      // An unparseable or empty draft falls back to the stored value.
      if (draft.trim() === '' || !Number.isFinite(parsed)) return;
      const clamped = Math.min(field.max, Math.max(field.min, Math.round(parsed)));
      if (clamped !== settings[field.key]) setField(field.key, clamped);
    },
    [drafts, settings, setField]
  );

  const runCheck = useCallback(async () => {
    setChecking(true);
    try {
      setCheckResult(await meetingDetectionService.probeSignals());
    } catch (error) {
      toast.error('Check failed', { description: errorText(error) });
    } finally {
      setChecking(false);
    }
  }, []);

  if (loadError) {
    return (
      <div className="space-y-3">
        <div className="flex items-start gap-3 p-4 bg-red-50 border border-red-200 rounded-lg text-sm text-red-800">
          <AlertCircle className="h-5 w-5 flex-shrink-0 mt-0.5" />
          <div>
            <p className="font-medium">Meeting detection is unavailable</p>
            <p className="mt-1 font-mono break-all">{loadError}</p>
          </div>
        </div>
        <button
          onClick={() => {
            setLoadError(null);
            setLoadAttempt((n) => n + 1);
          }}
          className="px-3 py-2 text-sm border border-gray-300 rounded-md hover:bg-gray-50 transition-colors"
        >
          Retry
        </button>
      </div>
    );
  }

  if (!settings) {
    return (
      <div className="animate-pulse">
        <div className="h-4 bg-gray-200 rounded w-1/4 mb-4"></div>
        <div className="h-8 bg-gray-200 rounded mb-4"></div>
      </div>
    );
  }

  const current = status?.current_meeting ?? null;

  return (
    <div className="space-y-6">
      <div>
        <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
          <Radar className="w-5 h-5 text-gray-600" />
          Auto Record
        </h3>
        <p className="text-sm text-gray-600 mb-6">
          Detects Google Meet, Microsoft Teams and Zoom calls and records them automatically. Detection uses which
          application is capturing the microphone (macOS) and Zoom&apos;s meeting process. All processing stays on this
          computer.
        </p>
      </div>

      {/* Master switch */}
      <div className="flex items-center justify-between p-4 border rounded-lg">
        <div className="flex-1">
          <Label htmlFor="md-enabled" className="font-medium">
            Enable meeting detection
          </Label>
          <div className="text-sm text-gray-600">
            Runs in the background while Meetily is open, including when it is only in the tray.
          </div>
        </div>
        <Switch
          id="md-enabled"
          aria-label="Enable meeting detection"
          checked={settings.enabled}
          disabled={saving}
          onCheckedChange={(checked) => setField('enabled', checked)}
        />
      </div>

      {/* Limitations: decision material for enabling, so it sits directly under the switch. */}
      <div className="p-4 border rounded-lg bg-yellow-50">
        <div className="text-sm text-yellow-800">
          <strong>Limitations:</strong> Google Meet is inferred from browser microphone use, so other web calls in the
          same browser also trigger recording. Microphone-based detection is macOS only. Recording requires a ready
          transcription model; if it is missing, the error appears below and a notification is shown.
        </div>
      </div>

      {/* Status */}
      <div className="p-4 border rounded-lg bg-gray-50 space-y-3">
        <div className="flex items-center justify-between">
          <div className="font-medium">Status</div>
          <div className="text-xs text-gray-500">Last check: {formatTime(status?.last_probe_at ?? null)}</div>
        </div>
        {!settings.enabled ? (
          <div className="text-sm text-gray-500">Detection is off.</div>
        ) : current ? (
          <div className="text-sm">
            <div className="text-green-700 font-medium">
              In meeting: {current.display_name} (since {formatTime(current.detected_at)})
            </div>
            <div className="text-gray-600">
              {current.recording_started_by_detector
                ? `Recording "${current.meeting_name}" started automatically.`
                : 'Recording was not started by the detector.'}
            </div>
            {status?.signal_lost_secs != null && (
              <div className="text-amber-700">
                Signal lost {status.signal_lost_secs}s ago. Stops after {settings.stop_grace_secs}s.
              </div>
            )}
          </div>
        ) : status?.pending_app ? (
          <div className="text-sm text-amber-700">
            {appLabel(status.pending_app)} signal seen ({status.pending_secs ?? 0}s). Confirming after{' '}
            {settings.start_confirm_secs}s.
          </div>
        ) : (
          <div className="text-sm text-gray-600">Waiting for a meeting.</div>
        )}
        <div>
          <div className="text-xs text-gray-500 mb-1">Current signals</div>
          <SignalList signals={status?.last_signals ?? []} />
        </div>
        {status?.probe_error && (
          <div className="flex items-start gap-2 text-sm text-red-700">
            <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
            <span>Detection is not working on this system: {status.probe_error}</span>
          </div>
        )}
        {status?.last_error && (
          <div className="flex items-start gap-2 text-sm text-red-700">
            <AlertCircle className="w-4 h-4 mt-0.5 flex-shrink-0" />
            <span>{status.last_error}</span>
          </div>
        )}
      </div>

      {/* Behaviour */}
      <div className="space-y-3">
        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex-1">
            <Label htmlFor="md-auto-start" className="font-medium">
              Start recording automatically
            </Label>
            <div className="text-sm text-gray-600">
              Meeting name: &quot;&lt;App&gt; YYYY-MM-DD HH-mm&quot;. Uses the microphone and system audio devices from
              Recordings.
            </div>
          </div>
          <Switch
            id="md-auto-start"
            aria-label="Start recording automatically"
            checked={settings.auto_start_recording}
            disabled={saving}
            onCheckedChange={(checked) => setField('auto_start_recording', checked)}
          />
        </div>
        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex-1">
            <Label htmlFor="md-auto-stop" className="font-medium">
              Stop recording automatically
            </Label>
            <div className="text-sm text-gray-600">
              Only recordings started by the detector are stopped. Manual recordings are left running.
            </div>
          </div>
          <Switch
            id="md-auto-stop"
            aria-label="Stop recording automatically"
            checked={settings.auto_stop_recording}
            disabled={saving}
            onCheckedChange={(checked) => setField('auto_stop_recording', checked)}
          />
        </div>
        <div className="flex items-center justify-between p-4 border rounded-lg">
          <div className="flex-1">
            <Label htmlFor="md-notify" className="font-medium">
              System notifications
            </Label>
            <div className="text-sm text-gray-600">
              Notify when a meeting is detected, recording starts, and recording stops.
            </div>
          </div>
          <Switch
            id="md-notify"
            aria-label="System notifications"
            checked={settings.notify_on_detection}
            disabled={saving}
            onCheckedChange={(checked) => setField('notify_on_detection', checked)}
          />
        </div>
      </div>

      {/* Apps */}
      <div className="p-4 border rounded-lg space-y-3">
        <div className="font-medium">Applications</div>
        {APP_TOGGLES.map(({ key, label, help }) => (
          <div key={key} className="flex items-center justify-between">
            <div className="flex-1">
              <Label htmlFor={`md-${key}`} className="text-sm font-medium">
                {label}
              </Label>
              <div className="text-xs text-gray-500">{help}</div>
            </div>
            <Switch
              id={`md-${key}`}
              aria-label={label}
              checked={settings[key]}
              disabled={saving}
              onCheckedChange={(checked) => setField(key, checked)}
            />
          </div>
        ))}
        <div className="pt-2">
          <Label htmlFor="md-bundle-ids" className="text-sm font-medium block mb-1">
            Browsers treated as Google Meet (macOS bundle IDs, prefix match)
          </Label>
          <Textarea
            id="md-bundle-ids"
            className="font-mono text-sm h-28"
            value={bundleIdsText}
            disabled={saving}
            onChange={(e) => setBundleIdsText(e.target.value)}
            onBlur={commitBundleIds}
            spellCheck={false}
          />
          <div className="text-xs text-gray-500 mt-1">
            One per line. Saved when the field loses focus. Helper processes such as com.google.Chrome.helper match
            their parent id.
          </div>
        </div>
      </div>

      {/* Timing */}
      <div className="p-4 border rounded-lg space-y-4">
        <div className="font-medium">Timing</div>
        {NUMBER_FIELDS.map((f) => (
          <div key={f.key} className="flex items-center justify-between gap-4">
            <div className="flex-1">
              <Label htmlFor={`md-${f.key}`} className="text-sm font-medium">
                {f.label}
              </Label>
              <div className="text-xs text-gray-500">{f.help}</div>
            </div>
            <Input
              id={`md-${f.key}`}
              type="number"
              inputMode="numeric"
              min={f.min}
              max={f.max}
              className="w-24 text-right"
              disabled={saving}
              value={drafts[f.key] ?? String(settings[f.key])}
              onChange={(e) => setDrafts((d) => ({ ...d, [f.key]: e.target.value }))}
              onBlur={() => commitNumber(f)}
            />
          </div>
        ))}
      </div>

      {/* Diagnostics */}
      <div className="p-4 border rounded-lg space-y-3">
        <div className="flex items-center justify-between">
          <div>
            <div className="font-medium">Diagnostics</div>
            <div className="text-xs text-gray-500">
              Runs one check with every application enabled, regardless of the switches above.
            </div>
          </div>
          <button
            onClick={runCheck}
            disabled={checking}
            className="flex items-center gap-2 px-3 py-2 text-sm border border-gray-300 rounded-md hover:bg-gray-50 transition-colors disabled:opacity-50"
          >
            <RefreshCw className={`w-4 h-4 ${checking ? 'animate-spin' : ''}`} />
            Check now
          </button>
        </div>
        {checkResult !== null && <SignalList signals={checkResult} />}
      </div>
    </div>
  );
}
