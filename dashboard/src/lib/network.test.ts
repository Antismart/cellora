import { afterEach, describe, expect, it } from 'vitest';

import { isNetwork, setNetwork } from './network';

afterEach(() => {
  window.localStorage.clear();
});

describe('isNetwork', () => {
  it('accepts known networks', () => {
    expect(isNetwork('mainnet')).toBe(true);
    expect(isNetwork('testnet')).toBe(true);
  });

  it('rejects anything else', () => {
    expect(isNetwork('devnet')).toBe(false);
    expect(isNetwork(undefined)).toBe(false);
    expect(isNetwork(null)).toBe(false);
    expect(isNetwork(42)).toBe(false);
  });
});

describe('setNetwork', () => {
  it('persists the choice to localStorage', () => {
    setNetwork('mainnet');
    expect(window.localStorage.getItem('cellora.network')).toBe('mainnet');
  });
});
