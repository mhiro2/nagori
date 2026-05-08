import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => false),
  subscribe: vi.fn(() => () => {}),
  TAURI_EVENTS: {
    navigate: 'nagori://navigate',
    pasteFailed: 'nagori://paste_failed',
    hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
  },
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
}));

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
