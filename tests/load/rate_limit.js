// Load test for the per-key Redis token-bucket rate limiter.
//
// Run against a running Cellora API + Postgres + Redis stack. Issue a
// free-tier API key via the admin CLI, then point the script at it:
//
//   $ cargo run -p cellora-api -- admin create-key --tier free --label load-test
//   $ export CELLORA_API_KEY=cell_...
//   $ export CELLORA_BASE_URL=http://localhost:8080
//   $ k6 run tests/load/rate_limit.js
//
// Default config: 60 VUs hammering `/v1/blocks/latest` for 10 seconds.
// With free-tier defaults (burst 30, refill 1/s), the limiter should
// allow ~40 requests over the window (30 burst + ~10 refilled); the
// rest of the ~6000 attempts return 429.
//
// Pass criteria, captured by the thresholds:
//   * No 5xx responses at all.
//   * At least one 200 (the bucket initialises full).
//   * At least one 429 (we are deliberately above the limit).
//   * Every 429 carries `Retry-After` and `X-RateLimit-Reset`.
//   * Every 200 carries `X-RateLimit-Remaining`.
//
// We deliberately do not assert exact 200/429 ratios — clock skew and
// Redis round-trip make strict ratios flaky. The shape ("most 429,
// some 200, zero 5xx, headers present") is what matters.

import http from 'k6/http';
import { Counter } from 'k6/metrics';
import { check } from 'k6';

const BASE_URL = __ENV.CELLORA_BASE_URL || 'http://localhost:8080';
const API_KEY = __ENV.CELLORA_API_KEY;

if (!API_KEY) {
    throw new Error(
        'CELLORA_API_KEY is required — issue one via `cargo run -p cellora-api -- admin create-key --tier free`'
    );
}

const ok_count = new Counter('rl_ok');
const limited_count = new Counter('rl_limited');
const bad_count = new Counter('rl_bad');

export const options = {
    vus: 60,
    duration: '10s',
    thresholds: {
        rl_bad: ['count==0'],
        rl_ok: ['count>0'],
        rl_limited: ['count>0'],
        // Headers must always accompany their respective statuses.
        'checks{kind:headers_on_429}': ['rate==1.0'],
        'checks{kind:headers_on_200}': ['rate==1.0'],
    },
};

export default function () {
    const response = http.get(`${BASE_URL}/v1/blocks/latest`, {
        headers: {
            authorization: `Bearer ${API_KEY}`,
        },
    });

    if (response.status === 200) {
        ok_count.add(1);
        check(
            response,
            {
                '200 has X-RateLimit-Remaining': (r) =>
                    r.headers['X-Ratelimit-Remaining'] !== undefined,
            },
            { kind: 'headers_on_200' }
        );
    } else if (response.status === 429) {
        limited_count.add(1);
        check(
            response,
            {
                '429 has Retry-After': (r) =>
                    r.headers['Retry-After'] !== undefined,
                '429 has X-RateLimit-Reset': (r) =>
                    r.headers['X-Ratelimit-Reset'] !== undefined,
            },
            { kind: 'headers_on_429' }
        );
    } else {
        bad_count.add(1);
    }
}
