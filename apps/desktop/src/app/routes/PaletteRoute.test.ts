import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

vi.mock('../components/Palette.svelte', () => ({
  default: vi.fn(),
}));

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => false),
}));

vi.mock('../lib/commands', () => ({
  // The route only mounts <Palette/>, but Palette imports the command surface
  // and the i18n + stores; stub everything to keep the route test isolated.
  closePalette: vi.fn(),
  refreshRecent: vi.fn(),
  refreshSettings: vi.fn(),
}));

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
