# Rawtonic contribution relay

The Cloudflare Worker the app's contribution channels send to (problem reports with an optional screenshot and photo file, crash reports, camera-test results, opt-in usage statistics). It fits Cloudflare's Workers Free plan: small JSON bodies, files streamed into R2 as raw bytes. Design, payloads and privacy rules: [ADR 0017](../../docs/decisions/0017-contribution-channels.md). Deployment and operation: [docs/RELAY.md](../../docs/RELAY.md).

```sh
npm ci
npm test          # vitest inside the Workers runtime (Miniflare), D1 and R2 in memory
npm run typecheck
```

`src/index.ts` routes and handles requests, `schema.ts` validates payloads, `limits.ts` holds rate limits, global caps and the proof of work, `stats.ts` the statistics counters, `issues.ts` the issue texts, `github.ts` the GitHub client; `migrations/` the D1 tables. No runtime dependencies: everything in `package.json` is a build or test tool.
