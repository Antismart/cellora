# Dashboard frontend — backend handoff

This document is for the **cellora-nuxt** dashboard team. It describes the Cellora HTTP surface the dashboard integrates against, the auth model for both the programmatic API (`/v1/*`) and the dashboard API (`/admin/*`), the response shapes, the headers clients are expected to handle, and what is and isn't shipped on the backend today.

The previous React/Vite scaffold has been archived under `docs/archive/dashboard-react-scaffold/`. The "What's worth lifting from the archive" section at the end of this doc points out the specific pieces (cookie name, env vars, network conventions, etc.) that the Nuxt build should match so the backend doesn't change shape twice.

---

## 1. Audiences and auth surfaces

Cellora exposes two HTTP surfaces with different auth models. They never share credentials.

| Surface     | Audience            | Auth                                 | Rate-limited? |
|-------------|---------------------|--------------------------------------|---------------|
| `/v1/*`     | Programmatic clients (user code) | `Authorization: Bearer cell_<prefix>_<secret>` | yes, per-key |
| `/graphql`  | Programmatic clients (user code) | same Bearer as `/v1/*`               | yes, per-key (separate bucket) |
| `/admin/*`  | Dashboard humans    | `cellora_session` `HttpOnly` cookie  | no            |
| public      | Anyone (probes, spec) | none                                | no            |

**The dashboard never holds, sees, or sends a Bearer token.** It runs against `/admin/*` exclusively. When the user wants to test the API from inside the dashboard ("API Explorer" page), the dashboard fetches the most-recently-used key through `/admin/keys` and forwards it as a Bearer to `/v1/*` from the browser — but that key is the user's own programmatic credential, not a dashboard credential.

---

## 2. Base URL, environments, CORS

| Env       | API base URL                       | Notes |
|-----------|------------------------------------|-------|
| local dev | `http://localhost:8080`            | started by `cargo run -p cellora-api` |
| staging   | (TBD — set by ops in Week 7)       | |
| prod      | (TBD — set by ops in Week 7)       | |

Expose this to the Nuxt app as `NUXT_PUBLIC_API_BASE_URL` (or whatever the Nuxt-side convention dictates). The previous React build called it `VITE_API_BASE_URL`; the value is identical, only the variable name changes.

**CORS is configurable on the API.** Set `CELLORA_API_CORS_ALLOWED_ORIGINS` to a comma-separated allowlist (e.g. `https://cellora.vercel.app`). The API will send `Access-Control-Allow-Credentials: true` so the session cookie is forwarded. If this env var is empty, the API is same-origin only.

For local dev today: run the Nuxt dev server on `http://localhost:3000` and proxy `/admin/*` and `/v1/*` to `http://localhost:8080` through Nuxt's dev proxy. Same-origin from the browser's perspective, no CORS needed.

---

## 3. The dashboard surface (`/admin/*`)

### Shipped today

| Method | Path        | Returns                  | Notes |
|--------|-------------|--------------------------|-------|
| GET    | `/admin/me` | `{ user: UserView }`     | 401 if no session cookie or session expired |

`UserView`:

```ts
type UserView = {
  id: string;            // UUID, stable opaque dashboard user id
  github_login: string;  // mutable upstream — re-fetched on every sign-in
  email: string | null;
  avatar_url: string | null;
};
```

### Not shipped — Nuxt should keep its mock fetchers until each lands

These endpoints back the rest of the Nuxt prototype. They're scoped into Week 5 slices but not yet implemented. The Nuxt team should keep mock fetchers (composables already in `cellora-nuxt`) and just swap the implementation when each endpoint lands. The contracts below are the planned shapes — comment back on this doc if anything needs to change before implementation.

| Method | Path                              | Purpose | Slice |
|--------|-----------------------------------|---------|-------|
| GET    | `/admin/oauth/github/start`       | Redirect to GitHub authorize URL | 3 |
| GET    | `/admin/oauth/github/callback`    | OAuth code exchange → upserts user, mints session cookie, 302 to `/app` | 3 |
| POST   | `/admin/sign-out`                 | Invalidates session row, clears cookie | 3 |
| GET    | `/admin/keys`                     | List the user's API keys | 5 |
| POST   | `/admin/keys`                     | Create key — returns `full` once | 5 |
| POST   | `/admin/keys/:id/rotate`          | Rotate (new secret, same row) | 5 |
| DELETE | `/admin/keys/:id`                 | Revoke | 5 |
| GET    | `/admin/usage`                    | Series + breakdown for charts | 6 |
| GET    | `/admin/usage/429s`               | Recent rate-limit refusals | 6 |
| GET    | `/admin/status`                   | Per-network indexer status (proxies `/v1/stats`) | 9 |

#### `POST /admin/keys` — reveal-once contract

Request body:

```json
{ "label": "production", "tier": "starter", "network": "mainnet" }
```

Response **on creation only**:

```json
{
  "key": {
    "id": "k_01h…",
    "label": "production",
    "tier": "starter",
    "network": "mainnet",
    "prefix": "cell_a1b2c3",
    "created_at": "2026-05-11T10:45:12Z",
    "status": "active"
  },
  "secret": "cell_a1b2c3_8f9e2…<64 hex chars>"
}
```

The `secret` field is **never returned again**. List/get responses omit it entirely. The Nuxt reveal-once modal (already built) is the correct UX — make sure the "I've saved this" confirm is the only path that dismisses it.

### Session cookie contract

The backend issues sessions in the OAuth callback. The cookie:

| Attribute  | Value              |
|------------|--------------------|
| name       | `cellora_session`  |
| value      | 32 random bytes, base64url-no-pad (the plaintext token; SHA-256 hash is what's stored) |
| `HttpOnly` | yes                |
| `Secure`   | yes (in prod; relaxed in dev only if you explicitly opt out) |
| `SameSite` | `Lax`              |
| `Path`     | `/`                |
| Max-Age    | (TBD with slice 3 — likely 30 days, rolling) |

**The frontend never reads, sets, or inspects this cookie.** Treat it as opaque. The only thing the Nuxt app does with auth state is:

1. On every page that requires sign-in, call `GET /admin/me`. 200 → user is signed in; render. 401 → redirect to `/sign-in`.
2. The sign-in page is a single button that hits `GET /admin/oauth/github/start`. The browser is then handed off to GitHub.
3. Sign-out is `POST /admin/sign-out`, then a client-side redirect to `/`.

This matches the existing `useAuth` composable in `cellora-nuxt` — the only change once OAuth lands is replacing the mock `signedIn` ref with a TanStack-Query-style `useQuery('/admin/me')` that the auth middleware reads.

---

## 4. The programmatic surface (`/v1/*` and `/graphql`)

The dashboard's **API Explorer** page hits these directly from the browser with the user's own bearer token (fetched from `/admin/keys`). Everything else on the dashboard goes through `/admin/*`.

### Public routes (no auth)

- `GET /v1/health` — liveness probe, returns 200 if process is up.
- `GET /v1/health/ready` — readiness probe, 200 if DB + Redis + CKB node all reachable, 503 otherwise.
- `GET /v1/openapi.json` — full OpenAPI 3 spec. **The Explorer's REST tab should embed Swagger UI pointed at this URL.**
- `GET /metrics` — Prometheus scrape, not for the dashboard.

### Authenticated REST routes

| Method | Path                                  | Purpose |
|--------|---------------------------------------|---------|
| GET    | `/v1/blocks/latest`                   | Highest-indexed block |
| GET    | `/v1/blocks/:number`                  | Block by number |
| GET    | `/v1/cells?lock_hash=…` or `?type_hash=…` | Cursor-paginated cells by lock/type hash |
| GET    | `/v1/stats`                           | Indexer tip, node tip, lag, staleness |
| GET    | `/v1/proofs/:tx_hash`                 | CKB `get_transaction_proof` passthrough |

### GraphQL

`POST /graphql` — same data through a single typed schema. Auth is the same Bearer. **The Explorer's GraphQL tab should embed GraphiQL pointed at this URL.**

Full schema, examples, and conventions live in [`docs/api.md`](./api.md). That doc is the source of truth; do not duplicate it in the dashboard. Link to it from the in-product help/footer instead.

---

## 5. Cross-cutting contracts the frontend must handle

### Error envelope (REST)

Every non-2xx response on the REST and admin surfaces returns:

```json
{
  "error": {
    "code": "bad_request",
    "message": "human-readable text",
    "details": null
  }
}
```

`code` is one of: `bad_request`, `not_found`, `invalid_cursor`, `unauthorized`, `rate_limited`, `upstream_unavailable`, `internal`.

Map these to user-facing UI:

| code                   | Status | Dashboard reaction |
|------------------------|--------|---------------------|
| `unauthorized`         | 401    | On `/admin/*`: redirect to `/sign-in`. On `/v1/*` (from Explorer): show "key revoked or expired" inline. |
| `rate_limited`         | 429    | Show countdown using `Retry-After` header value (seconds). |
| `bad_request`, `invalid_cursor` | 400 | Inline form error using `message`. |
| `not_found`            | 404    | Empty state for the page/resource. |
| `upstream_unavailable` | 503    | "Cellora is temporarily degraded — retrying" banner; back off and re-query. |
| `internal`             | 500    | Generic error toast + retry. Log `x-request-id` so users can quote it in support. |

### GraphQL errors

GraphQL uses its protocol-standard `{ data, errors }` shape on a **200 OK**. Auth (401) and rate-limit (429) errors still come back with their HTTP statuses **and** the REST envelope — those checks happen before the GraphQL handler runs. Branch on response shape, not status, when parsing.

### Headers every authed response carries

| Header                 | Meaning |
|------------------------|---------|
| `x-request-id`         | UUID — echoed from the client or generated. **Surface this in error toasts** so users can quote it. |
| `x-indexer-tip`        | Indexer tip block number at response time. Use it to show "synced to block N" without an extra fetch. |
| `x-indexer-tip-stale`  | `true` when the tip snapshot is older than the staleness threshold (default 5s). Show a warning chip. |
| `x-ratelimit-limit`    | Bucket capacity for the surface+tier. |
| `x-ratelimit-remaining`| Tokens left after this request. |
| `x-ratelimit-reset`    | Seconds until the bucket would refill from current state. |
| `retry-after`          | Set on 429 only — seconds until a token is available. |

The Explorer page should render `x-ratelimit-*` next to every response so users can sanity-check their tier. The hero on the landing page already uses `x-indexer-tip` to keep the live-tip pill honest.

### Cursor pagination

List endpoints return:

```json
{ "data": [...], "next_cursor": "eyJibiI6MTIzNDV…", "meta": { … } }
```

Cursors are opaque base64. **Don't parse them.** Pass `next_cursor` back as `?cursor=…`. A `null` `next_cursor` means the final page.

---

## 6. Multi-network model

`mainnet` and `testnet` are **first-class on the backend**. Every key, every usage event, and every list response is network-scoped.

### Where network lives

- **In the API URL.** Final shape lands with the key-management slice; the planned form is `/v1/{network}/...` and `/graphql/{network}`. Until then the API is implicitly single-network for whichever node it points at. Plan the Nuxt fetchers to pass network in the path now so the swap is mechanical.
- **In keys.** `api_keys.network` is a fixed column — a key issued for mainnet cannot query testnet. Show the network as a badge on every key row (already implemented in the prototype).
- **In sessions.** Sessions are user-scoped, **not** network-scoped. A signed-in user can switch networks freely; the active network is a client-side preference.

### Active-network convention (must match the archived scaffold)

| Key                       | Value      |
|---------------------------|------------|
| `localStorage` key        | `cellora.network` |
| Values                    | `"mainnet"` \| `"testnet"` |
| Default on first visit    | `"mainnet"` |

The `useNetwork()` composable in `cellora-nuxt` already implements this. **Do not change the localStorage key** — the archived React scaffold used the same name, and we want sign-in→sign-out→sign-in across implementations to preserve the user's choice.

Visually: every authed list row should carry a left-edge accent bar (brand green for mainnet, amber/testnet hue for testnet). The prototype gets this right via `.net-bar` and `.net-bar-test`.

---

## 7. Tiers, rate limits, and what to show on Pricing

Tier is fixed at key-creation time and drives both rate-limit buckets. These are the **default** values; ops can override via env vars (`CELLORA_API_RATE_LIMIT_*`).

| Tier    | REST burst | REST refill/s | GraphQL burst | GraphQL refill/s |
|---------|-----------:|--------------:|--------------:|-----------------:|
| free    |         30 |             1 |            10 |              0.5 |
| starter |        300 |            20 |           100 |               10 |
| pro     |      3,000 |           200 |         1,000 |              100 |

When showing usage charts:

- The ceiling for visualisation is `burst * 60` per minute (token bucket fully draining and refilling). The prototype computes this correctly in `pages/app/usage.vue`.
- Distinguish REST and GraphQL series — the buckets are separate. A user at 99% on REST can still be idle on GraphQL.

When Redis is unreachable, the limiter **fails open** by default (`CELLORA_API_RATE_LIMIT_FAIL_OPEN=true`). This is invisible to clients — they just see allowed responses with no `x-ratelimit-*` headers, or with stale values. Don't build UI around fail-open detection; it's an ops concern.

---

## 8. Concrete contracts for the prototype's mock fetchers

Each composable in `cellora-nuxt` maps to a backend endpoint. Use this table to land them as endpoints ship.

| Composable / mock      | Real fetch                                    | Status |
|------------------------|-----------------------------------------------|--------|
| `useAuth().user`       | `GET /admin/me`                                | ✅ shipped |
| `useAuth().signOut`    | `POST /admin/sign-out`                         | ⏳ slice 3 |
| `useLiveTip()`         | `GET /v1/stats` (via `/admin/status` once it exists, or directly if the dashboard origin can hit the API) | ⏳ slice 9 wraps; `/v1/stats` itself is shipped |
| `apiKeysSeed`          | `GET /admin/keys`                              | ⏳ slice 5 |
| `CreateKeyModal.onCreate` | `POST /admin/keys` → reveal `secret` once | ⏳ slice 5 |
| `onRotate`             | `POST /admin/keys/:id/rotate`                  | ⏳ slice 5 |
| `onRevoke`             | `DELETE /admin/keys/:id`                       | ⏳ slice 5 |
| `buildUsageSeries`     | `GET /admin/usage?range=24h|7d|30d&surface=…&key=…` | ⏳ slice 6 |
| `endpointBreakdown`    | included in `/admin/usage`                     | ⏳ slice 6 |
| `recent429s`           | `GET /admin/usage/429s`                        | ⏳ slice 6 |
| `statusByNetwork`      | `GET /admin/status` (proxies `/v1/stats` per network) | ⏳ slice 9 |
| Explorer "Run"         | direct browser-side fetch to `/v1/*` / `/graphql` with the picked key as Bearer | works today, just needs CORS on the API if Nuxt runs on a different origin |

---

## 9. What's worth lifting from the archived React scaffold

The archive lives at `docs/archive/dashboard-react-scaffold/`. It was never wired to real data — it shipped slice 1 (scaffold) and the backend half of slice 2 (`/admin/me`) only — so don't port code. **Do** port these conventions so the swap stays a stack change, not a contract change:

1. **Session cookie name and attributes** — `cellora_session`, `HttpOnly`, `Secure`, `SameSite=Lax`. The backend hard-codes this in `crates/api/src/session.rs`; the Nuxt app must not introduce a different name.
2. **`localStorage` key for active network** — `cellora.network`, values `"mainnet"` / `"testnet"`. Documented in the archived `dashboard/README.md` under "Networks".
3. **Env-var convention** — base URL is read at build time. The archived scaffold called it `VITE_API_BASE_URL`; Nuxt should call it `NUXT_PUBLIC_API_BASE_URL` with the same default of `http://localhost:8080`. The compose entry that used to wire this has been removed; ops will re-add an equivalent for the Nuxt project when deployment happens in Week 7.
4. **Dev proxy pattern** — the archived setup ran Vite on `:5173` and was expected to proxy to the API on `:8080` to keep cookies same-origin in dev. Nuxt should do the same via `nitro.devProxy` or `routeRules`, with both `/admin/*` and `/v1/*` proxied.
5. **The ADR is still valid where it covers product decisions** — ADR 0007 locks in GitHub-OAuth-only auth (no email/password) and multi-network as first-class. Those decisions don't move. The "Stack" section of that ADR (Vite/React/Tailwind/etc.) is now stale — a follow-up ADR will supersede it.
6. **The slice plan in ADR 0007 still drives the backend.** The Nuxt team can build pages in any order they prefer, but each `/admin/*` endpoint lands roughly in the slice listed in §3 above; coordinate if you need one earlier.

There is no useful component code in the archive. The Nuxt prototype's UI library is more complete than the React scaffold ever was.

---

## 10. Open questions / things to flag back to the backend team

The Nuxt team should push back on any of these if they don't work for the dashboard:

- **CORS** — not currently configured. Either we run same-origin via a reverse proxy in prod (preferred) or the API needs `tower-http::cors` with the dashboard origin allow-listed and `Access-Control-Allow-Credentials: true`. Decide before slice 3 ships.
- **Network in the URL vs. a header** — ADR 0007 leaves this open between `/v1/{network}/...` and `X-Cellora-Network: …`. The dashboard's Explorer page is what'll feel the difference most. Express a preference and we'll lock it.
- **Session cookie lifetime** — pick a max-age and a rolling-vs-absolute behaviour. Default proposal: 30-day rolling (every authenticated hit bumps `expires_at`).
- **Email visibility** — `/admin/me` currently surfaces the GitHub-reported email when GitHub returns one. Some users have it private. If the dashboard needs to render avatars more prominently than email, that's fine; if the dashboard depends on email everywhere, we need a UI for "set a contact email" too.
- **GraphQL inside the dashboard's admin surface** — currently `/admin/*` is REST only. If the dashboard wants to do a single batched query (e.g., keys + usage + status in one round trip) we can ship a GraphQL endpoint on the admin surface too. Not needed for v1.

Reply on any of these in the PR that adds the next slice's endpoint and we'll cut the decision into the backend ADR series.
