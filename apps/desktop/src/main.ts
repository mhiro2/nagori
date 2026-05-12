import { mount } from 'svelte';

import App from './app/App.svelte';
import { getSettings } from './app/lib/commands';
import { setLocale } from './app/lib/i18n/index.svelte';
import { isTauri } from './app/lib/tauri';
import { applyAppearance } from './app/lib/theme';

import './styles/app.css';

// Resolve the OS-preferred locale for the very first paint so a Japanese
// environment doesn't flash English while settings load. `'system'` does
// the negotiation internally and matches the default `AppSettings.locale`,
// so the round-trip below is a no-op when the user hasn't overridden it.
setLocale('system');

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
