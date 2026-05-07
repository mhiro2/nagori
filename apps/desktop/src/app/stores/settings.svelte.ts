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
  errorMessage: string | undefined;
};

export const settingsState = $state<SettingsStoreState>({
  settings: undefined,
  permissions: [],
  loaded: false,
  errorMessage: undefined,
});

export const refreshSettings = async (): Promise<void> => {
  if (!isTauri()) {
    settingsState.loaded = true;
    return;
  }
  try {
    const [s, p] = await Promise.all([getSettings(), getPermissions()]);
    settingsState.settings = s;
    settingsState.permissions = p;
    settingsState.errorMessage = undefined;
  } catch (err) {
    // Surface the failure so the settings/onboarding views can render a
    // banner instead of silently displaying defaults that don't reflect
    // the persisted config — a user with capture disabled would otherwise
    // see "capture: on" and assume their preference was honoured.
    settingsState.errorMessage = describeError(err);
  } finally {
    settingsState.loaded = true;
  }
};

export const captureEnabled = (): boolean => settingsState.settings?.captureEnabled ?? true;

export const aiEnabled = (): boolean => settingsState.settings?.aiEnabled ?? false;

export const accessibilityState = (): PermissionStatus | undefined =>
  settingsState.permissions.find((p) => p.kind === 'accessibility');

export const accessibilityGranted = (): boolean => accessibilityState()?.state === 'granted';
