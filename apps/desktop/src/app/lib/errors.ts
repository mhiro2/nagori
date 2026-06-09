import { messages } from '../lib/i18n/index.svelte';

const hasStringField = <K extends string>(value: object, key: K): value is Record<K, string> =>
  key in value && typeof Reflect.get(value, key) === 'string';

// True for the backend `settings_conflict` CommandError — the optimistic-
// concurrency check rejecting a stale `update_settings`. Callers recover by
// reloading the authoritative settings baseline and retrying rather than
// treating it as a hard failure.
export const isSettingsConflict = (err: unknown): boolean =>
  err !== null &&
  typeof err === 'object' &&
  'code' in err &&
  (err as { code: unknown }).code === 'settings_conflict';

export const describeError = (err: unknown): string => {
  const t = messages().errors;
  if (err && typeof err === 'object' && 'code' in err) {
    const code = (err as { code: unknown }).code;
    const fallback = hasStringField(err, 'message') ? err.message : t.unknown;
    switch (code) {
      case 'storage_error':
        return t.storage;
      case 'search_error':
        return t.search;
      case 'platform_error':
        return t.platform;
      case 'permission_error':
        return t.permission;
      case 'ai_error':
        return t.ai;
      case 'policy_error':
        return t.policy;
      case 'not_found':
        return t.notFound;
      case 'internal_error':
        // `internal_error` is the one variant whose backend message is built
        // from raw OS detail — absolute paths (`install_cli`), updater feed
        // URLs / signature-verification internals (`run_update`), symlink
        // targets, preview temp-dir paths. None of that is actionable for the
        // user and it leaks the local filesystem layout into the WebView, so
        // always show a generic sentence and leave the verbatim cause to the
        // backend `tracing` logs. Never fall through to the raw message.
        return t.internal;
      case 'forbidden':
        // `forbidden` messages are static, curated strings composed by the
        // command handler (e.g. "expanded preview is only available for Public
        // entries"); safe to surface verbatim, with a translation fallback when
        // the backend attached none.
        return hasStringField(err, 'message') && err.message.length > 0 ? err.message : t.forbidden;
      case 'paste_error':
        // Auto-paste failures carry an actionable, already-curated hint
        // ("install the `wtype` package", "Accessibility permission may be
        // missing"); no DB/SQL/path detail flows through this path, so the
        // message is safe to surface, with a generic fallback.
        return hasStringField(err, 'message') && err.message.length > 0 ? err.message : t.paste;
      case 'invalid_input':
        // Backend `invalid_input` payloads tend to be actionable (e.g. the
        // regex_denylist limit messages produced by `compile_user_regex`).
        // Surfacing the backend message verbatim keeps the user-facing hint
        // specific instead of squashing every input failure to a generic
        // "Invalid input." label. Fall back to the translation only if the
        // backend chose not to attach a message.
        return hasStringField(err, 'message') && err.message.length > 0
          ? err.message
          : t.invalidInput;
      case 'unsupported':
        // Prefer the backend-curated message (e.g. "auto-update is only
        // available on macOS", "Linux Wayland has no Accessibility settings
        // pane …") — the generic translation is the fallback when the
        // backend didn't supply one.
        return hasStringField(err, 'message') && err.message.length > 0
          ? err.message
          : t.unsupported;
      case 'configuration_error':
        return t.configuration;
      default:
        return fallback;
    }
  }
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  return t.unknown;
};
