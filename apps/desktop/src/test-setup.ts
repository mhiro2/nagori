// jsdom does not implement Element.scrollIntoView; ResultList relies on it
// to keep the active row in view, so stub it once for every test run rather
// than scattering polyfills across individual specs.
if (typeof Element !== 'undefined' && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = (): void => {};
}
