import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => {
  const { tauriMock } = await import('../test-helpers/moduleMocks');
  return tauriMock({ isTauri: vi.fn(() => false) });
});

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import SettingsRoute from './SettingsRoute.svelte';

afterEach(cleanup);

describe('SettingsRoute', () => {
  it('renders the SettingsView wrapper without throwing', () => {
    const { container } = render(SettingsRoute);
    expect(container).toBeTruthy();
  });
});
