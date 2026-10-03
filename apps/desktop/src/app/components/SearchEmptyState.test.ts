import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import SearchEmptyState from './SearchEmptyState.svelte';

afterEach(cleanup);

const props = () => ({
  query: '',
  filtered: false,
  loading: false,
  errorMessage: undefined,
  capturePaused: false,
  onRetry: vi.fn(),
  onClearSearch: vi.fn(),
});

describe('SearchEmptyState', () => {
  it('guides the first capture instead of suggesting a failed search', () => {
    const { getByRole, queryByRole } = render(SearchEmptyState, { props: props() });
    expect(getByRole('status').textContent).toContain('Copy some text to start your history.');
    expect(queryByRole('button')).toBeNull();
  });

  it.each([
    { query: 'missing', filtered: false, message: 'No entries match this search.' },
    { query: '', filtered: true, message: 'No entries match these filters.' },
  ])('offers to clear conditions after an empty result: $message', async (state) => {
    const callbacks = props();
    const { getByRole } = render(SearchEmptyState, { props: { ...callbacks, ...state } });
    expect(getByRole('status').textContent).toContain(state.message);
    await fireEvent.click(getByRole('button', { name: 'Clear search and filters' }));
    expect(callbacks.onClearSearch).toHaveBeenCalledOnce();
  });

  it('shows loading without briefly declaring the history empty or offering recovery', () => {
    const { getByRole, queryByRole } = render(SearchEmptyState, {
      props: { ...props(), query: 'pending', loading: true, errorMessage: 'previous failure' },
    });
    expect(getByRole('status').textContent).toContain('Searching…');
    expect(queryByRole('alert')).toBeNull();
    expect(queryByRole('button')).toBeNull();
  });

  it('announces a retrieval error and retries without clearing the search', async () => {
    const callbacks = props();
    const { getByRole } = render(SearchEmptyState, {
      props: { ...callbacks, query: 'keep me', errorMessage: 'Storage error.' },
    });
    expect(getByRole('alert').textContent).toContain('Storage error.');
    await fireEvent.click(getByRole('button', { name: 'Try again' }));
    expect(callbacks.onRetry).toHaveBeenCalledOnce();
    expect(callbacks.onClearSearch).not.toHaveBeenCalled();
  });

  it('explains paused capture when there are no search conditions', () => {
    const { getByRole } = render(SearchEmptyState, {
      props: { ...props(), capturePaused: true },
    });
    expect(getByRole('status').textContent).toContain('Capture is paused.');
  });
});
