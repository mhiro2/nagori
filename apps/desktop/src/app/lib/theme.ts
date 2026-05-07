import type { Appearance } from './types';

export const applyAppearance = (appearance: Appearance): void => {
  document.documentElement.dataset.theme = appearance;
};
