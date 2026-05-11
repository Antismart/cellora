import { Link } from 'react-router-dom';

export function NotFound() {
  return (
    <div className="flex flex-col items-start gap-3 pt-12">
      <span className="font-mono text-xs uppercase tracking-wide text-neutral-500">404</span>
      <h1 className="text-2xl font-semibold tracking-tight">Page not found</h1>
      <Link to="/" className="text-sm text-brand-500 hover:underline">
        Back to home
      </Link>
    </div>
  );
}
