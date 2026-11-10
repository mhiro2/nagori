import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

import { getAiAvailability } from '../lib/commands';
import { en } from '../lib/i18n/locales/en';
import type { AiAvailability, AiProviderKind, AiSettings, AppSettings } from '../lib/types';
import SettingsTabAi from './SettingsTabAi.svelte';

const aiSettings = (overrides: Partial<AiSettings> = {}): AiSettings => ({
  enabled: true,
  provider: 'appleNative',
  allowedActions: [],
  allowStreaming: true,
  requestTimeoutMs: 30000,
  semanticIndexEnabled: false,
  semanticIndexAcPowerOnly: true,
  imageTextIndexEnabled: false,
  onboardingDismissed: false,
  allowOpenaiFallbackPrompt: true,
  ...overrides,
});

// The tab reads only `settings.ai`; the rest of the DTO is irrelevant here.
const settingsWith = (ai: AiSettings): AppSettings => ({ ai }) as unknown as AppSettings;

const availability = (
  provider: AiProviderKind,
  overallStatus: AiAvailability['overallStatus'],
): AiAvailability => ({ provider, overallStatus, actions: [] });

const renderTab = (ai: AiSettings, savedRevision = 0) =>
  render(SettingsTabAi, {
    props: { settings: settingsWith(ai), t: en, scheduleSave: vi.fn(), savedRevision },
  });

const statusText = (): string => screen.getByTestId('ai-status').textContent?.trim() ?? '';

describe('SettingsTabAi status line', () => {
  beforeEach(() => {
    vi.mocked(getAiAvailability).mockReset();
  });
  afterEach(() => {
    cleanup();
  });

  it('reads Off without probing while the AI switch is off', () => {
    renderTab(aiSettings({ enabled: false }));
    expect(statusText()).toBe(`${en.settings.ai.status}: ${en.settings.ai.statusDisabled}`);
    expect(getAiAvailability).not.toHaveBeenCalled();
  });

  it('asks for a provider when AI is on but none is selected', async () => {
    vi.mocked(getAiAvailability).mockResolvedValue(availability('disabled', 'unavailable'));
    renderTab(aiSettings({ provider: 'disabled' }));
    expect(statusText()).toBe(`${en.settings.ai.status}: ${en.settings.ai.statusNoProvider}`);
  });

  it('shows Checking until the probe answers, then the probed state', async () => {
    const probe = Promise.withResolvers<AiAvailability>();
    vi.mocked(getAiAvailability).mockReturnValue(probe.promise);
    renderTab(aiSettings());
    expect(statusText()).toBe(`${en.settings.ai.status}: ${en.settings.ai.statusChecking}`);
    probe.resolve(availability('appleNative', 'available'));
    await waitFor(() =>
      expect(statusText()).toBe(`${en.settings.ai.status}: ${en.settings.ai.statusAvailable}`),
    );
  });

  it('says the state is unknown when the probe fails', async () => {
    vi.mocked(getAiAvailability).mockRejectedValue(new Error('ipc down'));
    renderTab(aiSettings());
    await waitFor(() =>
      expect(statusText()).toBe(`${en.settings.ai.status}: ${en.settings.ai.statusUnknown}`),
    );
  });

  it('does not report a probe of the previous settings as off', async () => {
    // The switch was just turned on and its save has not landed: the stored
    // settings still say AI is off.
    vi.mocked(getAiAvailability).mockResolvedValue(availability('appleNative', 'disabled'));
    renderTab(aiSettings());
    await waitFor(() => expect(getAiAvailability).toHaveBeenCalledTimes(1));
    expect(statusText()).toBe(`${en.settings.ai.status}: ${en.settings.ai.statusChecking}`);
  });
});
