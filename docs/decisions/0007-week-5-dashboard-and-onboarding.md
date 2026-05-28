# ADR 0007 — Week 5 dashboard and onboarding

- **Status:** Accepted
- **Date:** 2026-05-06
- **Context:** Week 5 of the roadmap in `CLAUDE.md` — build the operator
  dashboard so users can self-serve API keys, see usage, and explore the API.
  This is the gating piece for moving from private alpha (keys issued by hand
  via the admin CLI) to public beta.

## Decisions

### 1. Stack

- **Vite 6 + React 19 + TypeScript (strict)** for the frontend.
- **Tailwind CSS 4** via the `@tailwindcss/vite` plugin. Theme tokens live in
  CSS under `@theme`; no `tailwind.config.js`.
- **React Router 7** in data-router mode.
- **TanStack Query 5** for server state — caching, refetch, retries.
- **Vitest** + **Testing Library** for unit and component tests.
- **ESLint 9** flat config + **Prettier 3**.

The frontend lives in `dashboard/` at the repo root, alongside `crates/`.

### 2. Package manager

**pnpm** (pinned to 10.17 via the `packageManager` field). Faster installs,
smaller disk footprint than npm, and content-addressable store plays well
with the docker dev image.

### 3. Authentication

**GitHub OAuth only for MVP.** Matches the roadmap. Email/password is not
worth the support burden (password reset, email verification, abuse) for an
audience of CKB developers who already have GitHub accounts.

When the OAuth flow lands in the next slice it will be:

- Authorization-code flow, server-mediated.
- API exchanges the code for a token, calls the GitHub `/user` endpoint, and
  upserts a `users` row keyed by `github_user_id`.
- Session cookie issued by the API, signed with a server secret, `HttpOnly`,
  `Secure`, `SameSite=Lax`. Cookie carries a session ID that maps to a
  `sessions` row.
- The dashboard never sees a bearer token — only session cookies.

### 4. Multi-network from day one

**Yes — `mainnet` and `testnet` are both first-class.**

The cost of treating network as a second-class concern and retrofitting it
later is much higher than baking it in now: schema migrations on
`api_keys`, `usage_events`, and any future table; coordinated changes
across the API, dashboard, and any external integrations.

Implications:

- `api_keys.network` and `usage_events.network` columns added with the key
  management slice.
- API request path becomes `/v1/{network}/...` (or a `X-Cellora-Network`
  header — to be decided in the API slice). Existing routes will be aliased
  during transition.
- Dashboard persists the active network in `localStorage` under
  `cellora.network`, exposed via a `useNetwork()` hook that re-renders
  consumers when it changes.
- The indexer runs as one instance per network. docker-compose stays
  single-network for local dev (the dev node); staging and prod run two
  indexer Deployments.

### 5. Out of scope this week

Sticking to `CLAUDE.md`'s Week 5 boundaries:

- No Stripe or paid-tier billing (Week 7).
- No webhooks or GraphQL subscriptions (Week 6).
- No production deploy story for the dashboard. `Dockerfile.dev` only;
  the production multi-stage build lands with Week 7's deployment work.
- No email — sessions are GitHub-only and ephemeral.

## Slices for Week 5

In order:

1. **Scaffold** — `dashboard/` with Vite + React + TS + Tailwind, landing
   page, sign-in stub, 404, smoke tests, docker-compose entry under the
   `dashboard` profile. **(this PR)**
2. **Backend foundation** — `users` and `sessions` tables, session cookie
   middleware, `/admin/*` route group gated by session.
3. **GitHub OAuth** — OAuth app, callback handler, session issuance.
4. **Frontend auth** — sign-in flow wired to OAuth, protected route shell,
   sign-out.
5. **Key management** — link `api_keys` to `users` (via an `org_id` column,
   so a user can later belong to a team), add `network` column, build
   `/admin/keys` CRUD endpoints and the dashboard UI.
6. **Usage tracking** — `usage_events` written from the rate-limit
   middleware, daily aggregate table populated by a worker, `/admin/usage`
   query endpoint.
7. **Usage charts** — frontend chart page using TanStack Query +
   the chart library (recharts, decided in slice 7).
8. **API explorer** — embedded Swagger UI for REST and GraphiQL for
   GraphQL, both pointed at the user's most recent key.
9. **Indexer status panel** — consume existing `/v1/stats`, surface tip and
   lag.
10. **Wrap-up** — login → create key → make request → see usage e2e test,
    `docs/dashboard.md` with screenshots, README updates.

## Consequences

- New dependency surface area (Node + pnpm + browser tooling) alongside
  the Rust workspace. CI gains a separate dashboard job.
- Two new Postgres tables (`users`, `sessions`) and two columns added
  to `api_keys` (`user_id`, `network`).
- A `sessions` table means we now have authenticated state for two
  audiences — programmatic clients (bearer tokens against API keys) and
  dashboard users (session cookies). The two paths share no auth code.
