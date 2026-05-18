import { messages } from '../lib/i18n/index.svelte';

const hasStringField = <K extends string>(value: object, key: K): value is Record<K, string> =>
  key in value && typeof Reflect.get(value, key) === 'string';

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
