import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import PreviewBodyImage from './PreviewBodyImage.svelte';

const baseProps = {
  entryId: 'id-1',
  body: { type: 'image' as const, mimeType: 'image/png', byteCount: 1024, width: 32, height: 32 },
  altText: 'preview',
  unavailableText: 'Image unavailable.',
  loadingText: 'Loading image…',
};

afterEach(cleanup);

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
});
