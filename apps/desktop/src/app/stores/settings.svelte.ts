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

export const refreshSettings = async (): Promise<void> => {
  if (!isTauri()) {
    settingsState.loaded = true;
    return;
  }
  // `Promise.allSettled` so a transient failure on one side (e.g. the
  // permission probe is briefly unavailable while the OS reloads its TCC
  // database) doesn't discard the fresh value from the other side. The
  // old `Promise.all` collapsed both legs to the first rejection, which
  // left the UI showing stale defaults even when one half was current.
  const [settingsResult, permissionsResult] = await Promise.allSettled([
    getSettings(),
    getPermissions(),
  ]);

  if (settingsResult.status === 'fulfilled') {
    settingsState.settings = settingsResult.value;
    settingsState.settingsErrorMessage = undefined;
  } else {
    settingsState.settingsErrorMessage = describeError(settingsResult.reason);
  }

  if (permissionsResult.status === 'fulfilled') {
    settingsState.permissions = permissionsResult.value;
    settingsState.permissionsErrorMessage = undefined;
  } else {
    settingsState.permissionsErrorMessage = describeError(permissionsResult.reason);
  }

  const settingsFailed = settingsResult.status === 'rejected';
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

export const captureEnabled = (): boolean => settingsState.settings?.captureEnabled ?? true;

export const accessibilityState = (): PermissionStatus | undefined =>
  settingsState.permissions.find((p) => p.kind === 'accessibility');

export const accessibilityGranted = (): boolean => accessibilityState()?.state === 'granted';
