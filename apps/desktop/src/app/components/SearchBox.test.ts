import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { DEFAULT_LOCALE, setLocale } from '../lib/i18n/index.svelte';
import SearchBox from './SearchBox.svelte';

afterEach(() => {
  cleanup();
  setLocale(DEFAULT_LOCALE);
});

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
    const input = result.getByRole('combobox') as HTMLInputElement;
    await user.type(input, 'needle');
    expect(onInput).toHaveBeenLastCalledWith('needle');
  });

  it('auto-focuses the input on mount', () => {
    const { getByRole } = render(SearchBox, {
      props: { value: '', onInput: () => {} },
    });
    expect(document.activeElement).toBe(getByRole('combobox'));
  });

  it.each([
    ['en', undefined, 'Search history…'],
    ['ja', undefined, '履歴を検索…'],
    ['ja', 'Filter…', 'Filter…'],
  ] as const)(
    'with the %s locale and placeholder prop %s shows %s',
    (locale, placeholder, expected) => {
      setLocale(locale);
      const { getByRole } = render(SearchBox, {
        props: { value: '', onInput: () => {}, ...(placeholder ? { placeholder } : {}) },
      });
      expect((getByRole('combobox') as HTMLInputElement).placeholder).toBe(expected);
    },
  );
});
