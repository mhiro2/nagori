import { beforeEach, describe, expect, it } from 'vitest';

import {
  clearMultiSelect,
  isMultiSelected,
  multiSelectState,
  rangeSelectMulti,
  reconcileMultiSelect,
  selectAllMulti,
  toggleMultiSelect,
} from './searchMultiSelect.svelte';

beforeEach(() => {
  clearMultiSelect();
});

describe('toggleMultiSelect', () => {
  it('adds and then removes an id, leaving the set empty', () => {
    toggleMultiSelect('a');
    expect(isMultiSelected('a')).toBe(true);
    toggleMultiSelect('a');
    expect(isMultiSelected('a')).toBe(false);
    expect(multiSelectState.selected.size).toBe(0);
  });

  it('updates the anchor to the most recently toggled-on id', () => {
    toggleMultiSelect('a');
    toggleMultiSelect('b');
    expect(multiSelectState.anchor).toBe('b');
    // Toggling `b` off clears the anchor it points at.
    toggleMultiSelect('b');
    expect(multiSelectState.anchor).toBeUndefined();
  });
});

describe('rangeSelectMulti', () => {
  it('extends the set from the anchor to the target inclusive, in list order', () => {
    toggleMultiSelect('a'); // anchor = a
    rangeSelectMulti(['a', 'b', 'c', 'd'], 'c');
    expect(isMultiSelected('a')).toBe(true);
    expect(isMultiSelected('b')).toBe(true);
    expect(isMultiSelected('c')).toBe(true);
    expect(isMultiSelected('d')).toBe(false);
  });

  it('falls back to a single toggle when no anchor is set yet', () => {
    rangeSelectMulti(['a', 'b', 'c'], 'b');
    expect(isMultiSelected('b')).toBe(true);
    expect(isMultiSelected('a')).toBe(false);
  });
});

describe('selectAllMulti', () => {
  it('replaces the set with the full list and parks the anchor on the last id', () => {
    toggleMultiSelect('x');
    selectAllMulti(['a', 'b', 'c']);
    expect(multiSelectState.selected.size).toBe(3);
    expect(isMultiSelected('x')).toBe(false);
    expect(multiSelectState.anchor).toBe('c');
  });
});

describe('reconcileMultiSelect', () => {
  it('drops ids that no longer appear in the result list', () => {
    toggleMultiSelect('a');
    toggleMultiSelect('b');
    reconcileMultiSelect(['a']);
    expect(isMultiSelected('a')).toBe(true);
    expect(isMultiSelected('b')).toBe(false);
  });

  it('clears the anchor when the anchor id is no longer visible', () => {
    toggleMultiSelect('a');
    toggleMultiSelect('b'); // anchor = b
    reconcileMultiSelect(['a']);
    expect(multiSelectState.anchor).toBeUndefined();
  });

  it('is a no-op when every selected id is still visible', () => {
    toggleMultiSelect('a');
    toggleMultiSelect('b');
    const before = multiSelectState.selected;
    reconcileMultiSelect(['a', 'b', 'c']);
    expect(multiSelectState.selected).toBe(before);
  });
});
