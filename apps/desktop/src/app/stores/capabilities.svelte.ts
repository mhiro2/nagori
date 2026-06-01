// Platform capability snapshot. The palette gates the Quick Look shortcut
// on `previewQuickLook.status`; the settings Advanced tab has its own
// fetch path so it can re-render with fresh data on demand. Outside of
// Tauri (Storybook / browser preview) the snapshot stays `undefined` and
// the gated affordances stay hidden.

import { getCapabilities } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { PlatformCapabilities } from '../lib/types';

type CapabilitiesStoreState = {
  capabilities: PlatformCapabilities | undefined;
  loaded: boolean;
};

export const capabilitiesState = $state<CapabilitiesStoreState>({
  capabilities: undefined,
  loaded: false,
});

export const refreshCapabilities = async (): Promise<void> => {
  if (!isTauri()) {
    capabilitiesState.loaded = true;
    return;
  }
  try {
    capabilitiesState.capabilities = await getCapabilities();
  } catch {
    // Capabilities drive optional affordances only; a failure should not
    // block the palette from rendering. Leave the snapshot undefined so
    // the gated shortcuts stay inert.
  } finally {
    capabilitiesState.loaded = true;
  }
};

export const quickLookAvailable = (): boolean =>
  capabilitiesState.capabilities?.previewQuickLook.status === 'available';

// Whether model-backed AI actions have a wired backend on this host.
// Drives whether the desktop exposes any AI surface (the Settings AI tab,
// the action-menu AI actions). Currently only macOS ships an engine, but
// gating on the capability — not a hardcoded platform — keeps the door
// open: a host that wires a backend later lights AI up automatically.
// Stays `false` until the capability snapshot loads (and outside Tauri),
// so AI surfaces never flash on a platform that can't run them.
export const aiActionsSupported = (): boolean =>
  capabilitiesState.capabilities?.aiActions.status !== undefined &&
  capabilitiesState.capabilities.aiActions.status !== 'unsupported';
