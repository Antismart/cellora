import { useSyncExternalStore } from 'react';

export type Network = 'mainnet' | 'testnet';

const STORAGE_KEY = 'cellora.network';
const DEFAULT_NETWORK: Network = 'testnet';
const NETWORK_CHANGE_EVENT = 'cellora:network-change';

function readNetwork(): Network {
  if (typeof window === 'undefined') return DEFAULT_NETWORK;
  const value = window.localStorage.getItem(STORAGE_KEY);
  return isNetwork(value) ? value : DEFAULT_NETWORK;
}

function subscribe(callback: () => void): () => void {
  if (typeof window === 'undefined') return () => {};
  const onStorage = (event: StorageEvent) => {
    if (event.key === STORAGE_KEY) callback();
  };
  window.addEventListener('storage', onStorage);
  window.addEventListener(NETWORK_CHANGE_EVENT, callback);
  return () => {
    window.removeEventListener('storage', onStorage);
    window.removeEventListener(NETWORK_CHANGE_EVENT, callback);
  };
}

export function useNetwork(): Network {
  return useSyncExternalStore(subscribe, readNetwork, () => DEFAULT_NETWORK);
}

export function setNetwork(value: Network): void {
  window.localStorage.setItem(STORAGE_KEY, value);
  window.dispatchEvent(new Event(NETWORK_CHANGE_EVENT));
}

export function isNetwork(value: unknown): value is Network {
  return value === 'mainnet' || value === 'testnet';
}
