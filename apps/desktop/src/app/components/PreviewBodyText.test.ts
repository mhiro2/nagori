import { cleanup, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import PreviewBodyText from './PreviewBodyText.svelte';

afterEach(cleanup);

describe('PreviewBodyText', () => {
  it('marks query matches in code without dropping the syntax colouring', () => {
    const { container } = render(PreviewBodyText, {
      props: {
        text: 'let order_total = 1;\nreturn order_total;',
        language: 'rust',
        isCode: true,
        query: 'let order',
        currentMatch: 1,
      },
    });
    const marks = [...container.querySelectorAll('mark.match')];
    // The keyword keeps its grammar class inside the mark.
    expect(marks[0]?.classList.contains('kw')).toBe(true);
    expect(marks.map((m) => m.textContent)).toEqual(['let', 'order', 'order']);
    expect(marks.map((m) => m.getAttribute('data-match'))).toEqual(['0', '1', '2']);
    expect(container.querySelector('mark.current')?.getAttribute('data-match')).toBe('1');
    // The body text is untouched by the overlay.
    expect(container.querySelector('code')?.textContent).toBe(
      'let order_total = 1;return order_total;',
    );
  });
});
