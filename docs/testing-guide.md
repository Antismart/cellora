# Cellora Beta Testing Guide

This guide is for beta testers joining the Cellora early access program. Everything runs in your browser at [cellora-nuxt.vercel.app](https://cellora-nuxt.vercel.app). There is nothing to install or configure locally.

Cellora is a real-time indexer for the Nervos CKB blockchain. It continuously ingests block data and makes it queryable through a REST API and a GraphQL API. The dashboard is where you manage your API keys, monitor your usage, and test queries directly.

If anything in this guide does not match what you see, or something behaves unexpectedly, that is worth reporting. The bug report format is at the bottom.

---

## Getting Started

### Step 1: Sign in

Go to [cellora-nuxt.vercel.app](https://cellora-nuxt.vercel.app) and click **Get started** or **Sign in with GitHub** from anywhere on the landing page.

You will be taken through GitHub's standard OAuth flow. Cellora only reads your GitHub handle, avatar, and primary email. If you have already authorised the app previously, GitHub will redirect you straight back without prompting.

After sign-in you land on the **Overview** page.

### Step 2: Create your first API key

Navigate to **API Keys** in the left sidebar and click **Create your first key** (or **Create key** if you already have keys).

Fill in the form:

- **Label**: A name to help you identify the key later, e.g. `beta-test`
- **Tier**: Controls your rate limits (see the tier table below). Start with **Free** for general testing

Click **Create**. A modal will appear showing your full API key. **Copy it now.** The secret is shown exactly once and cannot be retrieved again. If you close this modal without copying, you will need to create a new key.

Your key looks like this:

```
cell_a1b2c3d4_8f9e2d...
```

Keep it somewhere safe for the duration of your testing session.

### Step 3: Explore the dashboard

Once you have a key, the full platform is open to you. The sidebar navigation has six main sections covered in detail below.

---

## Dashboard Pages

### Overview

The Overview is your starting point each time you sign in. It shows:

- **Indexer status tiles**: The current block height Cellora has indexed, the live chain tip from the CKB node, the lag between the two, and whether the data snapshot is fresh
- **Usage summary**: Total requests in the last 24 hours, a breakdown between REST and GraphQL, p95 response latency, and error rate
- **Recent activity**: The last 10 API requests made across all your keys, including the endpoint hit, the key used, the HTTP status returned, and the response time

The indexer tip tile updates live every two seconds. A small lag of 0 to 5 blocks is completely normal. If you see the lag growing steadily and not recovering, note it in your report.

---

### API Keys

This is where you manage all your programmatic credentials.

**The keys table** shows each key's label, masked prefix, tier, creation date, last used time, 24-hour request count, and status.

**Status values:**
- **Active**: Working normally
- **Revoked**: The key has been disabled and no longer authenticates

**Actions you can take from this page:**

**Rotate a key**: Opens a confirmation modal and generates a new secret for the same key. The old secret stops working and the new secret is shown once. Because authenticated requests are cached briefly, the old secret may continue to work for up to about 60 seconds before the change takes full effect.

**Revoke a key**: Disables the key. Clients using it will start receiving 401 responses within about 60 seconds (an internal auth cache means revocation is not instant). This action cannot be undone.

**Filtering**: Use the search box to find a key by label or prefix.

**Rate limit tiers** (these apply per key, with separate buckets for REST and GraphQL):

| Tier | REST burst | REST refill | GraphQL burst | GraphQL refill |
|---|---|---|---|---|
| Free | 30 requests | 1 per second | 10 requests | 0.5 per second |
| Starter | 300 requests | 20 per second | 100 requests | 10 per second |
| Pro | 3,000 requests | 200 per second | 1,000 requests | 100 per second |

Burst is how many requests you can fire in rapid succession before the limiter activates. Refill is how fast the bucket recovers after that. The tier is set at creation and cannot be changed on an existing key.

---

### Usage

The Usage page gives you a detailed view of how your API keys are performing.

At the top, select a **time range** (24h, 7d, or 30d) and optionally filter by a specific key or surface (REST, GraphQL, or both).

**What you'll see:**

- A stacked area chart showing REST and GraphQL request volume over time, with a line marking your tier's rate ceiling
- Aggregate stats: total requests, p95 latency for both surfaces, and the percentage of requests that were rate-limited (429s)
- A top endpoints table ranked by request volume
- A recent 429 events card listing the most recent rate-limit refusals with the endpoint, key, surface, timestamp, and retry-after value

---

### API Explorer

The Explorer lets you test both API surfaces directly in the browser without leaving the dashboard. It has two tabs.

**Before using either tab**, paste your API key into the key field at the top. The field masks the input for security.

#### REST tab

The left panel lists the available endpoints. Click any one to load it into the request builder. Fill in any required query parameters in the rows provided (you can add or remove rows as needed), then click **Send**.

The right panel shows:
- HTTP status code and whether it was a success or error
- Response latency in milliseconds
- Key response headers including `x-indexer-tip` and `x-ratelimit-remaining`
- The full JSON response body with syntax highlighting
- A copy button for the raw JSON

**Endpoints available in the Explorer:**

| Endpoint | What it returns |
|---|---|
| `GET /v1/blocks/latest` | The highest block Cellora has indexed |
| `GET /v1/blocks/{number}` | A specific block by number |
| `GET /v1/cells` | Cells matching a lock or type script hash (paginated) |
| `GET /v1/stats` | Indexer tip, node tip, lag, and staleness |

#### GraphQL tab

The left panel is a query editor. A sample query is pre-loaded to get you started. Edit it or replace it entirely, then click **Run query**.

The response panel works the same as the REST tab.

A sample query to try:

```graphql
{
  blocksLatest {
    number
    hash
    transactionsCount
  }
  stats {
    indexerTip
    nodeTip
    lagBlocks
    isStale
  }
}
```

---

### Status

The Status page shows the health of Cellora's infrastructure.

At the top there is a banner indicating whether all systems are operational. Below that, the status card shows:

- Live stats: indexer tip, node tip, lag, and snapshot age
- Node-level health for each indexer node, including sync status and per-node lag
- A recent reorgs section (chain reorganisations detected and rolled back)

If the status banner shows degraded service, check the lag on the status page and cross-reference with the `x-indexer-tip-stale` header you see in Explorer responses.

---

### Settings

The Settings page shows your profile: GitHub handle, avatar, and email address.

---

## A Note on Networks

Cellora serves both **Mainnet** and a **testnet**. Use the network switcher in the dashboard to choose which network you're querying; the Explorer and status views follow the selected network.

Your API keys work across the networks the service exposes — a key is not locked to a single network. Network-scoped keys (restricting a key to one network) are planned but not yet live, so treat any per-key network choice as informational for now.

---

## Specific Things Worth Testing

### Key lifecycle

- Create a key, copy the secret, and confirm that trying to view it again is not possible
- Create a key and immediately revoke it; verify that requests with that key return 401
- Rotate a key and verify that the old secret no longer works while the new one does

### Authentication

- Sign out, then try to access `/app/keys` directly in the URL bar; confirm you are redirected to sign-in
- From the API Explorer, try sending a request with no key in the field; the Explorer should show an error before sending
- Use an obviously invalid key (e.g. `cell_fake_key`) in the Explorer and confirm the response is 401 with a clear error message

### Error responses

All API errors follow a consistent shape:

```json
{
  "error": {
    "code": "not_found",
    "message": "block not found",
    "details": null
  }
}
```

Try these in the REST Explorer to confirm error handling is working:

- `GET /v1/blocks/latest` with no API key in the header field: expect 401 `unauthorized`
- `GET /v1/blocks/abc` (non-numeric): expect 400 `bad_request`
- `GET /v1/blocks/9999999` (a block far beyond the current tip): expect 404 `not_found`
- `GET /v1/cells` with neither `lock_hash` nor `type_hash`: expect 400 `bad_request`
- `GET /v1/cells` with a `lock_hash` that is not a valid 32-byte hex value: expect 400 `bad_request`

### Rate limiting

To observe the rate limiter with a Free-tier key, use the REST Explorer to fire requests at `GET /v1/blocks/latest` quickly and repeatedly. After the initial burst (30 requests), you should start seeing 429 responses. The Explorer will show the `retry-after` header value indicating how many seconds until the bucket refills.

On successful requests, check the `x-ratelimit-remaining` header value in the Explorer response panel and watch it decrease with each request.

### Pagination

When querying cells via `GET /v1/cells`, try setting `limit=2` to force pagination. The response will include a `next_cursor` field. Pass that value back as `cursor=...` to fetch the next page. When you reach the last page, `next_cursor` will be null.

---

## Response Headers Worth Noting

Every authenticated API response includes headers that are useful for testing:

| Header | What it tells you |
|---|---|
| `x-request-id` | Unique ID for that specific request. Include this in any bug report. |
| `x-indexer-tip` | The block height Cellora had indexed at the time of the response. |
| `x-indexer-tip-stale` | If `true`, the indexer snapshot is outdated. Not expected on a healthy stack. |
| `x-ratelimit-remaining` | Tokens left in your bucket for this request surface. |
| `x-ratelimit-reset` | Seconds until your bucket would refill to full from its current state. |
| `retry-after` | Present on 429 responses only. Seconds until you can retry. |

The Explorer surfaces `x-indexer-tip` and `x-ratelimit-remaining` automatically in the response panel. For other headers, use your browser's developer tools Network tab or a dedicated HTTP client like Insomnia or Postman with the key pasted in as a Bearer token.

---

## How to File a Bug Report

A good report makes the difference between a fast fix and a hard-to-reproduce mystery. Include:

1. **What you were doing**: The page you were on, the action you took, and the API key tier if relevant
2. **What you expected to happen**
3. **What actually happened**: Copy the full response body if it is an API error
4. **The `x-request-id` value**: Visible in the Explorer response panel or browser dev tools. This lets the team find the exact request in server logs
5. **A screenshot** if the issue is visual or in the dashboard UI

Share reports in the feedback channel or issue tracker the team has provided for this beta.
