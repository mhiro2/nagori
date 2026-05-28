import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import SearchBox from './SearchBox.svelte';

afterEach(cleanup);

describe('SearchBox', () => {
  it('delegates each typed character to onInput, ending with the full needle', async () => {
    const user = userEvent.setup();
    // SearchBox uses one-way `{value}` binding, so the parent owns the state.
    // Mirror that here by re-rendering with the latest value after each input,
    // otherwise the controlled input keeps snapping back to ''.
    let value = '';
    const onInput = vi.fn((next: string) => {
      value = next;
      void result.rerender({ value, onInput });
    });
    const result = render(SearchBox, { props: { value, onInput } });
    const input = result.getByRole('textbox') as HTMLInputElement;
    await user.type(input, 'needle');
    expect(onInput).toHaveBeenLastCalledWith('needle');
  });

  it('auto-focuses the input on mount', () => {
    const { getByRole } = render(SearchBox, {
      props: { value: '', onInput: () => {} },
    });
    expect(document.activeElement).toBe(getByRole('textbox'));
  });

  it('falls back to the locale placeholder when none is provided', () => {
    const { getByRole } = render(SearchBox, {
      props: { value: '', onInput: () => {} },
    });
    const input = getByRole('textbox') as HTMLInputElement;
    expect(input.placeholder.length).toBeGreaterThan(0);
  });

  it('uses the explicit placeholder prop when supplied', () => {
    const { getByPlaceholderText } = render(SearchBox, {
      props: { value: '', placeholder: 'Filter…', onInput: () => {} },
    });
    expect(getByPlaceholderText('Filter…')).toBeTruthy();
  });
});
