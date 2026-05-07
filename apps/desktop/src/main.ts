import { mount } from 'svelte';

import App from './app/App.svelte';
import { getSettings } from './app/lib/commands';
import { detectInitialLocale, setLocale } from './app/lib/i18n/index.svelte';
import { isTauri } from './app/lib/tauri';
import { applyAppearance } from './app/lib/theme';

import './styles/app.css';

// Negotiate first based on the user's browser preference so the very first
// paint isn't English-by-default in a Japanese environment. The persisted
// setting (when available) wins after the round-trip below.
setLocale(detectInitialLocale());

if (isTauri()) {
  void getSettings()
    .then((settings) => {
      setLocale(settings.locale);
      applyAppearance(settings.appearance);
      return undefined;
    })
    .catch(() => {
      // Settings load failed — keep the navigator-derived locale.
    });
}

const target = document.getElementById('app');
if (!target) {
  throw new Error('Mount target #app missing.');
}

mount(App, { target });
