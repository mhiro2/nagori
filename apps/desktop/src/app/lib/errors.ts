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
        return t.invalidInput;
      case 'unsupported':
        return t.unsupported;
      default:
        return fallback;
    }
  }
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  return t.unknown;
};
