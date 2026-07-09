import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';

import PreviewBodyImage from './PreviewBodyImage.svelte';

const baseProps = {
  entryId: 'id-1',
  body: { type: 'image' as const, mimeType: 'image/png', byteCount: 1024, width: 32, height: 32 },
  altText: 'preview',
  unavailableText: 'Image unavailable.',
  loadingText: 'Loading image…',
  platform: 'macos' as const,
};

afterEach(cleanup);

// jsdom has no layout, so getBoundingClientRect / scroll sizes are all zero by
// default. Pin a square frame and a fresh scroll origin so the zoom-anchor math
// (`(scroll + pointer) * ratio - pointer`) produces concrete, assertable
// offsets. jsdom does persist scrollLeft/scrollTop assignments.
function stubFrameGeometry(frame: HTMLElement, size = 200): void {
  frame.getBoundingClientRect = () => ({
    left: 0,
    top: 0,
    right: size,
    bottom: size,
    width: size,
    height: size,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  });
  frame.scrollLeft = 0;
  frame.scrollTop = 0;
}

describe('PreviewBodyImage', () => {
  it('shows the loading caption until the image decodes', () => {
    // jsdom never fires <img> load, so the overlay stands in for the gap while
    // a thumbnail is (re)generated — verifying the worded status is present.
    const { getByText, getByRole } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: false },
    });
    expect(getByText('Loading image…')).toBeTruthy();
    // The status uses role="status" so assistive tech announces the wait.
    expect(getByRole('status').textContent).toContain('Loading image…');
  });

  it('clears the loading caption once the image loads', async () => {
    const { getByRole, queryByText } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: false },
    });
    await fireEvent.load(getByRole('img'));
    expect(queryByText('Loading image…')).toBeNull();
  });

  it('shows the unavailable message when an expanded image errors', async () => {
    // Expanded mode treats the first error as terminal (no thumbnail retry),
    // so the clearer "unavailable" message surfaces immediately.
    const { getByRole, getByText } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    await fireEvent.error(getByRole('img'));
    expect(getByText('Image unavailable.')).toBeTruthy();
  });

  it('steps the zoom level with the zoom chord and refits on the reset chord', async () => {
    const { container, getByText } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    // The readout is always present while expanded — at fit it reads 100 %.
    expect(container.querySelector('.zoom-level')?.textContent).toBe('100%');
    // baseProps.platform is macOS, so the primary modifier is Cmd (metaKey).
    // First discrete step is 1.5× → 150 %.
    await fireEvent.keyDown(window, { key: '=', metaKey: true });
    expect(getByText('150%')).toBeTruthy();
    await fireEvent.keyDown(window, { key: '=', metaKey: true });
    expect(getByText('200%')).toBeTruthy();
    // `-` steps back down, the reset chord snaps back to fit (100 %).
    await fireEvent.keyDown(window, { key: '-', metaKey: true });
    expect(getByText('150%')).toBeTruthy();
    await fireEvent.keyDown(window, { key: '0', metaKey: true });
    expect(container.querySelector('.zoom-level')?.textContent).toBe('100%');
  });

  it('zooms with the chord regardless of which element has focus', async () => {
    // The chord types nothing into a field, so — unlike a bare key — it can
    // zoom even while the search box holds focus, never eating a search char.
    const { getByText } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();
    await fireEvent.keyDown(input, { key: '=', metaKey: true });
    expect(getByText('150%')).toBeTruthy();
    input.remove();
  });

  it('ignores a bare zoom key and only listens while expanded', async () => {
    const collapsed = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: false },
    });
    // No readout (and no listener) while collapsed.
    await fireEvent.keyDown(window, { key: '=', metaKey: true });
    expect(collapsed.container.querySelector('.zoom-level')).toBeNull();
    cleanup();
    const expanded = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    // A bare `=` (no modifier) is an ordinary search character — left alone, so
    // the zoom stays at fit (100 %).
    await fireEvent.keyDown(window, { key: '=' });
    expect(expanded.container.querySelector('.zoom-level')?.textContent).toBe('100%');
  });

  it('uses the platform primary modifier (Ctrl on non-mac)', async () => {
    const { getByText, container } = render(PreviewBodyImage, {
      props: { ...baseProps, platform: 'windows', expanded: true },
    });
    // Cmd (metaKey) is not the Windows primary modifier → ignored (stays 100 %).
    await fireEvent.keyDown(window, { key: '=', metaKey: true });
    expect(container.querySelector('.zoom-level')?.textContent).toBe('100%');
    // Ctrl is.
    await fireEvent.keyDown(window, { key: '=', ctrlKey: true });
    expect(getByText('150%')).toBeTruthy();
  });

  it('zooms with Ctrl/Cmd + wheel but pans on a plain wheel', async () => {
    const { container } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    // A plain wheel pans (native overflow scroll) — it must not zoom (stays 100 %).
    await fireEvent.wheel(frame, { deltaY: -100 });
    expect(container.querySelector('.zoom-level')?.textContent).toBe('100%');
    // Ctrl + wheel up zooms in (the cross-platform pinch / ctrl-scroll path).
    await fireEvent.wheel(frame, { deltaY: -100, ctrlKey: true });
    const level = container.querySelector('.zoom-level');
    expect(level).not.toBeNull();
    expect(level?.textContent).not.toBe('100%');
  });

  it('toggles fit ↔ 2× on double-click', async () => {
    const { container, getByText } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    await fireEvent.dblClick(frame);
    expect(getByText('200%')).toBeTruthy();
    await fireEvent.dblClick(frame);
    expect(container.querySelector('.zoom-level')?.textContent).toBe('100%');
  });

  it('zooms on a trackpad pinch (WebKit gesturechange) relative to its start', async () => {
    const { container, getByText } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    frame.dispatchEvent(new Event('gesturestart', { cancelable: true }));
    const change = new Event('gesturechange', { cancelable: true });
    (change as unknown as { scale: number }).scale = 2;
    frame.dispatchEvent(change);
    await tick();
    expect(getByText('200%')).toBeTruthy();
  });

  it('re-pins the scroll offset toward the cursor on a double-click zoom', async () => {
    // Anchor the zoom on the gesture point: double-clicking the bottom-right
    // corner must scroll there, not grow the image off the top-left corner.
    const { container } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    stubFrameGeometry(frame, 200);
    await fireEvent.dblClick(frame, { clientX: 200, clientY: 200 });
    await tick();
    // ratio 2× (fit → 2×): (0 + 200) * 2 - 200 = 200 — the clicked point stays put.
    expect(frame.scrollLeft).toBeCloseTo(200, 4);
    expect(frame.scrollTop).toBeCloseTo(200, 4);
  });

  it('anchors a pointer-less keyboard zoom on the frame centre', async () => {
    const { container } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    stubFrameGeometry(frame, 200);
    await fireEvent.keyDown(window, { key: '=', metaKey: true }); // → 1.5×
    await tick();
    // Centre anchor (px = py = 100): (0 + 100) * 1.5 - 100 = 50 on each axis.
    expect(frame.scrollLeft).toBeCloseTo(50, 4);
    expect(frame.scrollTop).toBeCloseTo(50, 4);
  });

  it('scales the stage by the continuous zoom while the readout rounds', async () => {
    // Guards the anchor against drift: the stage must scale by the unrounded
    // zoom so its size matches the scroll-correction ratio. If the stage used
    // the rounded percent, a sub-percent step would move scroll without moving
    // the stage and slowly slide the anchored point.
    const { container } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: true },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    await fireEvent.wheel(frame, { deltaY: -100, ctrlKey: true });
    await tick();
    const stage = container.querySelector('.image-stage') as HTMLElement;
    // 1.0015^100 ≈ 1.1617 → the stage var carries the fractional percent…
    const pct = Number.parseFloat(stage.style.getPropertyValue('--zoom-pct'));
    expect(pct).toBeCloseTo(116.17, 1);
    expect(Number.isInteger(pct)).toBe(false);
    // …while the corner readout rounds it to a whole percent.
    expect(container.querySelector('.zoom-level')?.textContent).toBe('116%');
  });

  it('does not attach pinch / wheel zoom while collapsed', async () => {
    const { container } = render(PreviewBodyImage, {
      props: { ...baseProps, expanded: false },
    });
    const frame = container.querySelector('.image-frame') as HTMLElement;
    await fireEvent.wheel(frame, { deltaY: -100, ctrlKey: true });
    await fireEvent.dblClick(frame);
    expect(container.querySelector('.zoom-level')).toBeNull();
  });
});
