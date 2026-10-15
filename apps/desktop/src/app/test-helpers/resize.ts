// Stand-in for the ResizeObserver jsdom lacks (installed by `test-setup.ts`).
// Svelte's `bind:clientWidth` — the palette's narrow-layout switch — observes
// through it. It never fires on its own, which leaves widths unmeasured (0,
// the wide layout); `resizeElement` delivers a resize on demand.

const observers = new Set<StubResizeObserver>();

export class StubResizeObserver implements ResizeObserver {
  readonly targets = new Set<Element>();

  constructor(readonly callback: ResizeObserverCallback) {
    observers.add(this);
  }

  observe(target: Element): void {
    this.targets.add(target);
  }

  unobserve(target: Element): void {
    this.targets.delete(target);
  }

  disconnect(): void {
    this.targets.clear();
  }
}

/// Pin `element`'s measured width and notify the observers watching it.
export const resizeElement = (element: HTMLElement, clientWidth: number): void => {
  Object.defineProperty(element, 'clientWidth', { configurable: true, value: clientWidth });
  const rect = element.getBoundingClientRect();
  const entry: ResizeObserverEntry = {
    target: element,
    contentRect: rect,
    borderBoxSize: [],
    contentBoxSize: [],
    devicePixelContentBoxSize: [],
  };
  for (const observer of observers) {
    if (observer.targets.has(element)) observer.callback([entry], observer);
  }
};
