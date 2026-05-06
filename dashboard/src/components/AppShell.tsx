import { Link, Outlet } from 'react-router-dom';

import { NetworkBadge } from '@/components/NetworkBadge';

export function AppShell() {
  return (
    <div className="flex min-h-full flex-col">
      <header className="border-b border-neutral-900/80 bg-neutral-950/60 backdrop-blur">
        <div className="mx-auto flex w-full max-w-6xl items-center justify-between px-6 py-4">
          <Link to="/" className="flex items-center gap-2">
            <span className="inline-block h-2.5 w-2.5 rounded-full bg-brand-500" aria-hidden />
            <span className="font-semibold tracking-tight">Cellora</span>
          </Link>
          <nav className="flex items-center gap-3 text-sm text-neutral-400">
            <NetworkBadge />
            <Link
              to="/signin"
              className="rounded-md border border-neutral-800 px-3 py-1.5 text-neutral-200 transition hover:border-neutral-700 hover:bg-neutral-900"
            >
              Sign in
            </Link>
          </nav>
        </div>
      </header>
      <main className="mx-auto w-full max-w-6xl flex-1 px-6 py-12">
        <Outlet />
      </main>
      <footer className="border-t border-neutral-900/80 px-6 py-6 text-center text-xs text-neutral-600">
        Cellora is open source under FSL-1.1-ALv2.
      </footer>
    </div>
  );
}
