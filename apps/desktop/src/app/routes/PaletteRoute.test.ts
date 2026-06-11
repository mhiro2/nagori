import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('../components/Palette.svelte', () => ({
  default: vi.fn(),
}));

vi.mock('../lib/tauri', async () => {
  const { tauriMock } = await import('../test-helpers/moduleMocks');
  return tauriMock({ isTauri: vi.fn(() => false) });
});

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

import PaletteRoute from './PaletteRoute.svelte';

afterEach(cleanup);

describe('PaletteRoute', () => {
  it('renders without throwing', () => {
    const { container } = render(PaletteRoute);
    // The mock makes <Palette/> a noop component; we only assert the route
    // mounts cleanly so the wrapper module is exercised.
    expect(container).toBeTruthy();
  });
});
