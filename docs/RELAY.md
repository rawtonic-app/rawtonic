# Deploying the contribution relay

The relay (`services/relay/`, design: [ADR 0017](decisions/0017-contribution-channels.md)) is the one service the app's contribution channels talk to: problem reports (with an optional screenshot and the photo's file), crash reports, camera-test results and opt-in usage statistics. RAW sample donations do not go through it: the app points to raw.pixls.us. It is a Cloudflare Worker with a D1 database and an R2 bucket, and it files reports as issues in a **private** GitHub repository. Deploying it is a maintainer's action; nothing in the app sends until a build carries the relay's URL.

What runs where, and what it costs: the relay runs on Cloudflare's **Workers Free plan** (user decision 2026-10-24: no paid plan). Its limits that matter here: **10 ms of CPU time per request** (time spent waiting on D1, R2 or GitHub does not count), 100,000 requests a day, 50 subrequests per request, a request body of at most 100 MB; D1's free 5 GB; R2's free 10 GB of storage. The relay is built for them (whether it stays within the CPU limit is measured on Cloudflare, not in the tests: read it after every deploy, next section): JSON bodies are at most 96 KiB, and a report's files (screenshot ≤ 2 MiB, photo file ≤ 100 MiB) are sent afterwards as raw bytes (`PUT /upload/:token`) that the runtime pipes into R2 without JavaScript reading them — only a screenshot's first 8 bytes are looked at (PNG or JPEG), and R2 itself checks the SHA-256 the report announced. The scheduled run stays within the per-run limits (below). Check the CPU time after every deploy (next section). No third-party service besides Cloudflare and GitHub is involved. The relay has no runtime npm dependencies — `wrangler`, `vitest` and the type packages in `services/relay/package.json` are build and test tools and are not deployed (they are listed in the npm inventory of `THIRD_PARTY_NOTICES.md`).

## CPU time per request

The Free plan stops a request that uses more than its CPU time (error 1102), so look at it after each deploy and when traffic changes:

- **Dashboard:** Workers & Pages → `rawtonic-relay` → **Metrics** → *CPU time*: percentiles (median, P99, P99.9) per invocation over the chosen period, without any request data. The P99.9 must stay well below 10 ms. *Errors* on the same page counts requests stopped for exceeding it. The **Logs** tab stays empty: Workers Logs are off on purpose (`observability` in `wrangler.jsonc`), since they would store IP addresses and bodies.
- **Live, per request:** `npx wrangler tail rawtonic-relay --format json` in `services/relay` while sending a test report; each entry has `cpuTime` and `wallTime` in milliseconds. Tail shows the request's headers, including the sender's IP address, in your terminal only (nothing is stored): use it for your own test sends, keep it short, and never paste its output anywhere.

## Once

1. **Accounts.** A Cloudflare account on the Workers Free plan and the GitHub organisation `rawtonic-app`.
2. **Private repository** for the reports, e.g. `rawtonic-app/rawtonic-reports` — **private**, issues on, everything else off. Create the labels `report`, `crash` and `camera-test` (Issues → Labels).
3. **Bot account and token.** Create a separate GitHub account for the relay (e.g. `rawtonic-relay-bot`, its own e-mail, two-factor on), invite it to the reports repository only, with the **Admin** role: deleting an issue (the user's delete route, and retention) needs repository admin rights; nothing less removes an issue with its edit history. As the bot, create a token limited to that repository: a fine-grained token with *Issues: read and write* (the organisation must allow fine-grained tokens), or a classic token with the `repo` scope if deleting fails with the fine-grained one (check in step 9). Give it an expiry and put a reminder in the calendar.
4. **Tools.** Node 24+, then in the repository:
   ```sh
   cd services/relay
   npm ci
   npx wrangler telemetry disable   # wrangler's own usage reporting
   npx wrangler login               # opens the browser for the Cloudflare account
   ```
5. **Database.**
   ```sh
   npx wrangler d1 create rawtonic-relay
   ```
   Paste the printed `database_id` into `wrangler.jsonc` (it names the account's database; it is not a secret), then create the tables:
   ```sh
   npx wrangler d1 migrations apply rawtonic-relay --remote
   ```
6. **Bucket** (screenshots and photo files of reports). Keep it private: do not turn on public access or an `r2.dev` URL.
   ```sh
   npx wrangler r2 bucket create rawtonic-contributions
   ```
7. **Repository name.** If the reports repository is not `rawtonic-app/rawtonic-reports`, change `GITHUB_REPO` in `wrangler.jsonc`.
8. **Secrets** — never in git, never in `wrangler.jsonc`:
   ```sh
   npx wrangler secret put GITHUB_TOKEN   # the bot's token from step 3
   openssl rand -hex 32 | npx wrangler secret put IP_HASH_KEY
   openssl rand -hex 32 | tee /dev/tty | npx wrangler secret put ADMIN_TOKEN   # keep this one in the password manager: it reads the statistics
   ```
9. **Deploy and test.**
   ```sh
   npx wrangler deploy
   ```
   It serves `https://relay.rawtonic.cc` (the custom domain in `wrangler.jsonc`; the domain's zone is on the same Cloudflare account) and `https://rawtonic-relay.<your-subdomain>.workers.dev`. Then file a report with a screenshot, upload the screenshot, and delete both:
   ```sh
   RELAY=https://relay.rawtonic.cc
   printf '\211PNG\r\n\032\nsmoke test' > /tmp/smoke.png   # a PNG signature is enough
   SIZE=$(wc -c < /tmp/smoke.png | tr -d ' '); SHA=$(shasum -a 256 /tmp/smoke.png | cut -d' ' -f1)
   curl -sS -X POST "$RELAY/report" -H 'content-type: application/json' -H 'x-rawtonic-client: smoke-test' \
     -d '{"schema":2,"app":{"version":"0.0.0","os":"macos","osVersion":"27.0","arch":"aarch64"},"text":"Relay smoke test","screenshot":{"mime":"image/png","size":'"$SIZE"',"sha256":"'"$SHA"'"}}'
   # → {"id":"…","ref":"…","uploads":{"screenshot":"https://relay.rawtonic.cc/upload/…"},"expiresAt":"…"};
   #   an issue "Problem report <ref>" appears in the private repository
   curl -sS -o /dev/null -w '%{http_code}\n' -X PUT --data-binary @/tmp/smoke.png \
     -H 'content-type: application/octet-stream' "<the screenshot upload URL>"
   # → 204; `npx wrangler r2 object get rawtonic-contributions/reports/<hash>/screenshot.png --remote --pipe` shows it
   curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE "$RELAY/report/<id>"
   # → 204, and the issue is gone (not just closed), the screenshot too
   ```
   If the delete answers 502, the token cannot delete issues (step 3: Admin role, or a classic `repo` token). Then read the CPU time of these requests (section above).
10. **Build the app with the relay.** The relay URL is fixed at build time: optimized builds (`npm run tauri -- build`, the release packages) use `https://relay.rawtonic.cc`; `RAWTONIC_RELAY_URL=https://…` names another, and an empty `RAWTONIC_RELAY_URL=` builds without one. Debug builds and tests have none unless it is set. A build without a relay has no contribution channels. Release smoke tests check that the packaged binary contains the URL (alpha.4 shipped without it).

## Operating

- **Statistics** (only values counted at least 10 times in that week; the rest is one `(other)` line):
  ```sh
  curl -sS "$RELAY/stats/summary?week=2026-W43" -H "authorization: Bearer $ADMIN_TOKEN"
  ```
- **A report's files.** The issue names their keys (`reports/<hash>/screenshot.png`, `reports/<hash>/photo.<file type>`; "if uploaded": a file whose upload did not finish is missing). Fetch one:
  ```sh
  npx wrangler r2 object get "rawtonic-contributions/reports/<hash>/photo.CR3" --remote --file report-photo.CR3
  ```
  A photo file is the sender's private photo, sent only to debug that report: keep it out of git, out of shared folders and out of test fixtures (AGENTS.md: no photographs in the repository; ADR 0017), and delete your copy when the report is done. It is deleted from the bucket with the report after 90 days.
- **Bucket size:** `npx wrangler r2 bucket info rawtonic-contributions` (objects and bytes). R2's free 10 GB hold about a hundred 100 MB photo files at the global cap; when it nears 8 GB, see the second fallback below.
- **Retention** runs every 10 minutes (cron in `wrangler.jsonc`), in batches small enough for the Free plan's per-run limits: records and bucket objects (the files) of reports, crashes and camera tests go after 90 days (~2,000 a day), and issues in the reports repository are deleted 90 days after they were created, oldest first (~2,800 a day) — so do not keep anything else in that repository. Check what is stored: `npx wrangler d1 execute rawtonic-relay --remote --command "SELECT kind, state, COUNT(*) FROM submissions GROUP BY kind, state"`.
- **Migrations on deploy:** `0002_sweep_cursor.sql` (the bucket walk's cursor) must be applied with `npx wrangler d1 migrations apply rawtonic-relay --remote` before deploying.
- **Spam.** Set `POW_BITS` in `wrangler.jsonc` and deploy; requests without a proof of work with that many bits then get 403. The app always sends a 20-bit stamp (`crates/contribute`, `POW_BITS`), so any value up to 20 works with released apps; a higher one needs an app update first. Limits and global caps are in `services/relay/src/limits.ts`.
- **Secrets.** Rotate the GitHub token before it expires (`npx wrangler secret put GITHUB_TOKEN`). Changing `IP_HASH_KEY` only resets the current rate-limit windows.
- **Changes.** `npm test` and `npm run typecheck` in `services/relay` (also part of `scripts/check.sh`); a new migration goes into `migrations/` and is applied with the command from step 5 before deploying. A changed payload is a new `schema` number (ADR 0017).
- **Shutting down.** `npx wrangler delete`, then delete the D1 database and the R2 bucket in the dashboard, and the reports repository on GitHub; remove the URL from the app's build.

## Fallbacks on the maintainers' own server (deskmini)

If the Free plan stops being enough, the relay moves to the deskmini; the app needs no change for either way, because it only knows `https://relay.rawtonic.cc` and the protocol, and both stay the same.

**1. The whole relay on the deskmini, behind a free Cloudflare Tunnel.** The same code runs in `workerd`, the runtime Cloudflare uses, through `wrangler dev`; D1 becomes a SQLite file and R2 a folder on the local disk, with no CPU limit. Cloudflare still terminates TLS and passes the sender's address as `CF-Connecting-IP` (the rate limits keep working), and its 100 MB upload limit stays the same.

1. On the deskmini: Node 24+, a checkout of the repository, `npm ci` in `services/relay`, and the secrets in `services/relay/.dev.vars` (`GITHUB_TOKEN=…`, `IP_HASH_KEY=…`, `ADMIN_TOKEN=…`; mode 600, never in git; a new `IP_HASH_KEY` only resets the current rate-limit windows).
2. Carry the database over (its export holds the tables, the applied migrations and the rows), or create empty tables:
   ```sh
   STATE=/srv/rawtonic-relay/state
   npx wrangler d1 export rawtonic-relay --remote --output relay.sql          # on a machine logged in to Cloudflare
   npx wrangler d1 execute rawtonic-relay --local --persist-to "$STATE" --file relay.sql
   # or, to start empty:
   npx wrangler d1 migrations apply rawtonic-relay --local --persist-to "$STATE"
   ```
   The bucket's objects (`reports/…`) go over with `npx wrangler r2 object get … --remote` and `npx wrangler r2 object put … --local --persist-to "$STATE"`, key by key (`objects` in the `submissions` rows lists them); with few reports, letting the 90-day retention empty the old bucket instead is simpler.
3. Run it as a service (systemd), listening on the loopback interface only:
   ```sh
   npx wrangler dev --local --ip 127.0.0.1 --port 8787 --persist-to "$STATE" --test-scheduled --log-level warn
   ```
   `--log-level warn` keeps request lines (with report ids in `DELETE /report/<id>`) out of the journal. `wrangler dev` is a development server: besides the relay it answers `/__scheduled` and a local explorer under `/cdn-cgi/local/explorer/` that reads the database and the bucket, and it keeps request traces (method and timing) in `$STATE/v3/observability`. So the tunnel below passes only the relay's own routes, and two systemd timers run on the loopback interface: the retention run every 10 minutes, `curl -fsS 'http://127.0.0.1:8787/__scheduled?cron=*/10+*+*+*+*'`, and once a day `curl -fsS -X POST http://127.0.0.1:8787/cdn-cgi/local/explorer/api/local/observability/clear`. (Tested on a laptop with `wrangler` 4.152: statistics stored and summarised, a report filed up to the GitHub call, the retention run through `/__scheduled`.)
4. Tunnel: `cloudflared tunnel create rawtonic-relay`, with an ingress that passes the relay's routes only:
   ```yaml
   ingress:
     - hostname: relay.rawtonic.cc
       path: ^/(report|crash|camera-test|stats|upload)(/[a-z2-7]+)?$
       service: http://127.0.0.1:8787
     - service: http_status:404
   ```
5. Switch over: remove the Worker's custom domain (the `routes` entry in `wrangler.jsonc`, then `npx wrangler deploy`, or in the dashboard), then `cloudflared tunnel route dns rawtonic-relay relay.rawtonic.cc`. Smoke-test as in step 9 above, then `npx wrangler delete` the Worker once nothing is left in its D1 and R2.

**2. Only the photo files on the deskmini, when R2's free 10 GB fill.** The Worker stays on Cloudflare and keeps the database, the issues, the screenshots and the protocol; only photo files (by far the largest) go to a small file store on the deskmini, reached through a Cloudflare Tunnel protected by a Cloudflare Access service token (free plan), so only the Worker can write to it. This needs a relay change, not an app change: a `PHOTO_STORE_URL` variable (and the Access token as a secret) under which `PUT /upload/:token` for a photo forwards the request body with `fetch` (streamed by the runtime like the R2 put, so the CPU time stays as low), deletion and retention send `DELETE` for photo keys there, and the store checks the size and SHA-256 the Worker passes along before keeping a file. Not built yet: build it when the bucket nears 8 GB (`r2 bucket info` above), or move to the first fallback instead.
