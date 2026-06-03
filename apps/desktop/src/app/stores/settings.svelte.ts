// Settings + permissions store. The palette UI shows live capture / AI flags
// in its status bar, and the onboarding banner needs the latest permission
// snapshot from `get_permissions`. Both are loaded lazily from the Tauri
// backend; outside of Tauri we surface defaults so the UI remains demoable.

import { getPermissions, getSettings } from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { AppSettings, PermissionStatus } from '../lib/types';

type SettingsStoreState = {
  settings: AppSettings | undefined;
  permissions: PermissionStatus[];
  loaded: boolean;
  // Global failure banner: only set when *both* IPC calls reject, i.e. the
  // store has nothing fresh to display. A partial failure leaves this
  // untouched so the side that did succeed isn't masked behind an error
  // overlay.
  errorMessage: string | undefined;
  // True when one of the two refresh legs failed but the other succeeded.
  // The Palette / Settings views key off this to render a compact
  // "some data is stale" badge while still binding the live half.
  partial: boolean;
  // Per-leg error text for the badge / debug tooling. Each is `undefined`
  // when its leg succeeded on the most recent refresh.
  settingsErrorMessage: string | undefined;
  permissionsErrorMessage: string | undefined;
};

export const settingsState = $state<SettingsStoreState>({
  settings: undefined,
  permissions: [],
  loaded: false,
  errorMessage: undefined,
  partial: false,
  settingsErrorMessage: undefined,
  permissionsErrorMessage: undefined,
});

// Bumped whenever an authoritative `settings_changed` snapshot is adopted.
// `refreshSettings` samples it before its async `getSettings` and discards its
// own settings write-back if the counter moved meanwhile — otherwise a slow
// in-flight read (kicked off on focus or at mount) could land *after* a fresher
// broadcast and clobber it with a stale value, silently reverting row count /
// preview / hotkeys until the next change. The permissions leg has no such
// broadcast, so it is never gated.
let settingsGeneration = 0;

export const refreshSettings = async (): Promise<void> => {
  if (!isTauri()) {
    settingsState.loaded = true;
    return;
  }
  const generation = settingsGeneration;
  // `Promise.allSettled` so a transient failure on one side (e.g. the
  // permission probe is briefly unavailable while the OS reloads its TCC
  // database) doesn't discard the fresh value from the other side. The
  // old `Promise.all` collapsed both legs to the first rejection, which
  // left the UI showing stale defaults even when one half was current.
  const [settingsResult, permissionsResult] = await Promise.allSettled([
    getSettings(),
    getPermissions(),
  ]);

  // A `settings_changed` snapshot that landed while this read was in flight
  // already wrote a fresher value, so leave the settings leg alone — both the
  // value and its (cleared) error are owned by `applySettingsSnapshot` now.
  const settingsSuperseded = settingsGeneration !== generation;
  if (!settingsSuperseded) {
    if (settingsResult.status === 'fulfilled') {
      settingsState.settings = settingsResult.value;
      settingsState.settingsErrorMessage = undefined;
    } else {
      settingsState.settingsErrorMessage = describeError(settingsResult.reason);
    }
  }

  if (permissionsResult.status === 'fulfilled') {
    settingsState.permissions = permissionsResult.value;
    settingsState.permissionsErrorMessage = undefined;
  } else {
    settingsState.permissionsErrorMessage = describeError(permissionsResult.reason);
  }

  // A superseded settings leg counts as fresh-and-good for banner purposes —
  // the snapshot supplied a usable value, so only a rejection that still owns
  // the current settings is a real failure.
  const settingsFailed = !settingsSuperseded && settingsResult.status === 'rejected';
  const permissionsFailed = permissionsResult.status === 'rejected';

  if (settingsFailed && permissionsFailed) {
    // Both legs failed — nothing landed, so route the user to the global
    // banner. Prefer the settings error since that's the wider blast
    // radius (capture/paste flags), falling back to permissions if
    // somehow only that side produced a message.
    settingsState.errorMessage =
      settingsState.settingsErrorMessage ??
      settingsState.permissionsErrorMessage ??
      describeError(settingsResult.reason);
    settingsState.partial = false;
  } else {
    settingsState.errorMessage = undefined;
    settingsState.partial = settingsFailed || permissionsFailed;
  }
  settingsState.loaded = true;
};

// Adopt a backend-published settings snapshot (the `settings_changed` event
// payload) without a round-trip through `getSettings`. Settings runs in its
// own webview, so the palette only learns of a change through this broadcast;
// applying the payload directly keeps `settingsState`-driven surfaces (row
// count, preview pane, palette hotkeys, paste-format default) live instead of
// stale until the next launch. The payload is authoritative for the settings
// leg, so its error clears; the independent permissions leg is left untouched
// (it refreshes on focus), and the global both-legs-failed banner demotes to
// the per-leg partial badge when only permissions are still stale.
//
// `loaded` is deliberately NOT advanced here: that flag also gates the
// permission-driven accessibility toast, which keys off a real permission
// snapshot. Letting a snapshot flip `loaded` true before the first
// `refreshSettings` lands its permissions would seed that toast from an empty
// `NotRequested` and flash a spurious ✓ once the genuine grant arrives. The
// generation bump lets a `refreshSettings` racing this call detect that a
// fresher snapshot won and keep its permissions leg without reverting settings.
export const applySettingsSnapshot = (next: AppSettings): void => {
  settingsGeneration += 1;
  settingsState.settings = next;
  settingsState.settingsErrorMessage = undefined;
  settingsState.errorMessage = undefined;
  settingsState.partial = settingsState.permissionsErrorMessage !== undefined;
};

export const captureEnabled = (): boolean => settingsState.settings?.captureEnabled ?? true;

export const accessibilityState = (): PermissionStatus | undefined =>
  settingsState.permissions.find((p) => p.kind === 'accessibility');

export const accessibilityGranted = (): boolean => accessibilityState()?.state === 'granted';
