import { render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { describe, expect, it } from 'vitest';

import { Landing } from './Landing';

describe('Landing', () => {
  it('renders the hero headline', () => {
    render(
      <MemoryRouter>
        <Landing />
      </MemoryRouter>,
    );
    expect(
      screen.getByRole('heading', { level: 1, name: /production-grade indexer/i }),
    ).toBeInTheDocument();
  });

  it('links the primary CTA to sign-in', () => {
    render(
      <MemoryRouter>
        <Landing />
      </MemoryRouter>,
    );
    const cta = screen.getByRole('link', { name: /get an api key/i });
    expect(cta).toHaveAttribute('href', '/signin');
  });
});
