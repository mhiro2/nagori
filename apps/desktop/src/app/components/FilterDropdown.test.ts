import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import FilterDropdown from './FilterDropdown.svelte';

afterEach(cleanup);

const multiProps = (onSelect: () => void) => ({
  label: 'Type',
  active: false,
  menuLabel: 'Type',
  multi: true,
  onSelect,
  items: [
    { value: 'url', label: 'URL', selected: false },
    { value: 'code', label: 'Code', selected: true },
  ],
});

describe('FilterDropdown', () => {
  it('disables the trigger when there are no items', () => {
    const { getByRole } = render(FilterDropdown, {
      props: {
        label: 'App',
        active: false,
        menuLabel: 'App',
        multi: false,
        items: [],
        onSelect: vi.fn(),
      },
    });
    expect(getByRole('button', { name: 'App' }).hasAttribute('disabled')).toBe(true);
  });

  it('opens on click and renders checkbox items with aria-checked', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(FilterDropdown, { props: multiProps(vi.fn()) });
    expect(getByRole('button', { name: 'Type' }).getAttribute('aria-expanded')).toBe('false');

    await user.click(getByRole('button', { name: 'Type' }));
    expect(getByRole('button', { name: 'Type' }).getAttribute('aria-expanded')).toBe('true');
    expect(getByRole('menuitemcheckbox', { name: 'URL' }).getAttribute('aria-checked')).toBe(
      'false',
    );
    expect(getByRole('menuitemcheckbox', { name: 'Code' }).getAttribute('aria-checked')).toBe(
      'true',
    );
  });

  it('multi-select keeps the menu open and forwards the chosen value', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    const { getByRole, queryByRole } = render(FilterDropdown, { props: multiProps(onSelect) });

    await user.click(getByRole('button', { name: 'Type' }));
    await user.click(getByRole('menuitemcheckbox', { name: 'URL' }));
    expect(onSelect).toHaveBeenCalledWith('url');
    // Still open for further toggles.
    expect(queryByRole('menu')).not.toBeNull();
  });

  it('single-select commits and closes the menu', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    const { getByRole, queryByRole } = render(FilterDropdown, {
      props: {
        label: 'App',
        active: false,
        menuLabel: 'App',
        multi: false,
        onSelect,
        items: [
          { value: 'chrome', label: 'Chrome', selected: false },
          { value: 'slack', label: 'Slack', selected: false },
        ],
      },
    });

    await user.click(getByRole('button', { name: 'App' }));
    expect(getByRole('menuitemradio', { name: 'Chrome' })).toBeDefined();
    await user.click(getByRole('menuitemradio', { name: 'Chrome' }));
    expect(onSelect).toHaveBeenCalledWith('chrome');
    expect(queryByRole('menu')).toBeNull();
  });

  it("folds the active selection into the trigger's accessible name", () => {
    const { getByRole } = render(FilterDropdown, {
      props: {
        label: 'URL',
        active: true,
        menuLabel: 'Type',
        multi: true,
        onSelect: vi.fn(),
        items: [
          { value: 'url', label: 'URL', selected: true },
          { value: 'code', label: 'Code', selected: false },
        ],
      },
    });
    // Active → the value is announced, not just the axis name.
    expect(getByRole('button', { name: 'Type: URL' })).toBeDefined();
  });

  it('closes on Escape without forwarding a selection', async () => {
    const onSelect = vi.fn();
    const user = userEvent.setup();
    const { getByRole, queryByRole } = render(FilterDropdown, { props: multiProps(onSelect) });

    await user.click(getByRole('button', { name: 'Type' }));
    await user.keyboard('{Escape}');
    expect(queryByRole('menu')).toBeNull();
    expect(onSelect).not.toHaveBeenCalled();
  });
});
