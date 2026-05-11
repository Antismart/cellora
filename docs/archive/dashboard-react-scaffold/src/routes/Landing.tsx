import { Link } from 'react-router-dom';

export function Landing() {
  return (
    <div className="flex flex-col gap-16">
      <section className="flex flex-col gap-6 pt-8">
        <span className="inline-flex w-fit items-center gap-2 rounded-full border border-neutral-800 bg-neutral-900/60 px-3 py-1 text-xs uppercase tracking-wide text-neutral-400">
          <span className="inline-block h-1.5 w-1.5 rounded-full bg-brand-500" aria-hidden />
          CKB indexer · Public beta soon
        </span>
        <h1 className="max-w-3xl text-4xl font-semibold leading-tight tracking-tight text-neutral-50 sm:text-5xl">
          A production-grade indexer for the Nervos CKB blockchain.
        </h1>
        <p className="max-w-2xl text-lg leading-relaxed text-neutral-400">
          Query cells, transactions, and blocks over REST and GraphQL. Reorg-safe, observable,
          multi-tenant. Built for teams shipping on CKB.
        </p>
        <div className="flex flex-wrap items-center gap-3">
          <Link
            to="/signin"
            className="rounded-md bg-brand-500 px-4 py-2 text-sm font-medium text-neutral-950 transition hover:bg-brand-400"
          >
            Get an API key
          </Link>
          <a
            href="https://github.com/Antismart/cellora"
            target="_blank"
            rel="noreferrer"
            className="rounded-md border border-neutral-800 px-4 py-2 text-sm text-neutral-200 transition hover:border-neutral-700 hover:bg-neutral-900"
          >
            View on GitHub
          </a>
        </div>
      </section>

      <section className="grid grid-cols-1 gap-6 sm:grid-cols-3">
        <Feature
          title="Reorg-safe ingestion"
          body="Parent-hash walk on every new block. Affected blocks, cells, and transactions roll back in a single Postgres transaction."
        />
        <Feature
          title="REST + GraphQL"
          body="The same data exposed two ways. Cursor-paginated cell queries, tagged with well-known script labels."
        />
        <Feature
          title="Multi-tenant from day one"
          body="Per-key Redis token-bucket rate limiting, separate quotas for REST and GraphQL, three tiers."
        />
      </section>
    </div>
  );
}

function Feature({ title, body }: { title: string; body: string }) {
  return (
    <article className="rounded-lg border border-neutral-900 bg-neutral-950/40 p-5">
      <h3 className="mb-2 text-sm font-semibold tracking-tight text-neutral-100">{title}</h3>
      <p className="text-sm leading-relaxed text-neutral-400">{body}</p>
    </article>
  );
}
