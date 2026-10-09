import { StubResizeObserver } from './app/test-helpers/resize';

// jsdom does not implement Element.scrollIntoView; ResultList relies on it
// to keep the active row in view, so stub it once for every test run rather
// than scattering polyfills across individual specs.
if (typeof Element !== 'undefined' && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = (): void => {};
}

// jsdom has no ResizeObserver either; see `app/test-helpers/resize`.
if (typeof globalThis.ResizeObserver === 'undefined') {
  globalThis.ResizeObserver = StubResizeObserver;
}
