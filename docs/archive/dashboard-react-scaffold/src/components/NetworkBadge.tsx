import { useNetwork } from '@/lib/network';

export function NetworkBadge() {
  const network = useNetwork();
  const dotClass =
    network === 'mainnet' ? 'bg-emerald-400' : 'bg-amber-400';
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-md border border-neutral-800 bg-neutral-900/60 px-2 py-1 font-mono text-xs uppercase tracking-wide text-neutral-300"
      aria-label={`Active network: ${network}`}
    >
      <span className={`inline-block h-1.5 w-1.5 rounded-full ${dotClass}`} aria-hidden />
      {network}
    </span>
  );
}
