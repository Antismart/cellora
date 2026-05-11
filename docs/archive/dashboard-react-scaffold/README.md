# Cellora dashboard

Operator and customer dashboard for the Cellora CKB indexer. Users sign in with
GitHub, manage API keys, view usage, and explore the API.

## Stack

- **Vite 6** + **React 19** + **TypeScript** (strict)
- **Tailwind CSS 4** via the Vite plugin (config-in-CSS, `@theme`)
- **React Router 7** (data router)
- **TanStack Query 5** for server state
- **Vitest** + **Testing Library** for tests
- **ESLint 9** (flat config) + **Prettier 3**

## Prerequisites

- Node.js 20+ (the repo is built and tested against Node 22)
- pnpm 10 (`corepack enable` will activate the version pinned in `package.json`)

## Getting started

```sh
cp .env.example .env.local
pnpm install
pnpm dev
```

The dev server binds on `http://localhost:5173`. Set `VITE_API_BASE_URL` to
the URL of your local Cellora API (defaults to `http://localhost:8080`).

## Scripts

| Script              | Purpose                                              |
| ------------------- | ---------------------------------------------------- |
| `pnpm dev`          | Vite dev server with HMR                             |
| `pnpm build`        | Type-check then produce a production bundle          |
| `pnpm preview`      | Serve the production bundle locally                  |
| `pnpm typecheck`    | `tsc -b --noEmit` across both project references     |
| `pnpm lint`         | ESLint with `--max-warnings=0`                       |
| `pnpm format`       | Prettier write                                       |
| `pnpm format:check` | Prettier check (use this in CI)                      |
| `pnpm test`         | Vitest run (jsdom environment, Testing Library)      |
| `pnpm test:watch`   | Vitest watch mode                                    |

## Project layout

```
dashboard/
├── src/
│   ├── components/      # Cross-route UI (AppShell, NetworkBadge, …)
│   ├── lib/             # Pure helpers, hooks, the API client
│   ├── routes/          # One file per route component (+ co-located tests)
│   ├── test/            # Vitest setup
│   ├── index.css        # Tailwind import + theme tokens
│   └── main.tsx         # Entry — router + QueryClient provider
├── eslint.config.js     # Flat ESLint config
├── tsconfig.app.json    # App TS project (strict, path alias @/*)
├── tsconfig.node.json   # Tooling TS project
└── vite.config.ts       # Vite + Vitest config
```

## Conventions

- **Path alias `@/*` → `src/*`** — use it in imports.
- **No default exports** for components — named exports only, makes refactors
  and tooling unambiguous.
- **One component per file**. Tests live next to the file under test
  (`Landing.tsx` → `Landing.test.tsx`).
- **No `any`**. Prefer `unknown` and narrow.
- **No comments explaining what code does** — only why, where the why is
  non-obvious. Same rule as the Rust crates.

## Networks

The dashboard treats `mainnet` and `testnet` as first-class. The active
network is persisted in `localStorage` under the key `cellora.network`. The
`useNetwork()` hook returns the current selection and re-renders consumers
when it changes. API requests must include the network in the request path
(`/v1/{network}/...`) — backend routing for that arrives with the key
management slice.
