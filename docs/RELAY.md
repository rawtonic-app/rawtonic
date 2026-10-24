# Deploying the contribution relay

The relay (`services/relay/`, design: [ADR 0017](decisions/0017-contribution-channels.md)) is the one service the app's contribution channels talk to: problem reports, crash reports, camera-test results, RAW sample donations and opt-in usage statistics. It is a Cloudflare Worker with a D1 database and an R2 bucket, and it files reports as issues in a **private** GitHub repository. Deploying it is a maintainer's action; nothing in the app sends until a build carries the relay's URL.

What runs where, and what it costs: use the **Workers Paid plan** (USD 5 a month; D1 and R2 within its included amounts at Rawtonic's volumes). The Free plan allows 10 ms of CPU per request, too little for a problem report with a screenshot (decoding, hashing and parsing up to 3 MB; not measured on Cloudflare yet). The scheduled run stays within the Free plan's per-run limits anyway. A request body may be at most 100 MB on both plans, which is the RAW sample limit. No third-party service besides Cloudflare and GitHub is involved. The relay has no runtime npm dependencies — `wrangler`, `vitest` and the type packages in `services/relay/package.json` are build and test tools and are not deployed (they are listed in the npm inventory of `THIRD_PARTY_NOTICES.md`).

## Once

1. **Accounts.** A Cloudflare account on the Workers Paid plan (above) and the GitHub organisation `rawtonic-app`.
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
6. **Bucket** (screenshots and RAW samples). Keep it private: do not turn on public access or an `r2.dev` URL.
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
   It prints the URL, `https://rawtonic-relay.<your-subdomain>.workers.dev` (a custom domain can be added in the Cloudflare dashboard: Workers → rawtonic-relay → Settings → Domains). Then file and delete one report:
   ```sh
   RELAY=https://rawtonic-relay.<your-subdomain>.workers.dev
   curl -sS -X POST "$RELAY/report" -H 'content-type: application/json' -H 'x-rawtonic-client: smoke-test' \
     -d '{"schema":1,"app":{"version":"0.0.0","os":"macos","osVersion":"27.0","arch":"aarch64"},"text":"Relay smoke test"}'
   # → {"id":"…","ref":"…"}; an issue "Problem report <ref>" appears in the private repository
   curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE "$RELAY/report/<id>"
   # → 204, and the issue is gone (not just closed)
   ```
   If the delete answers 502, the token cannot delete issues (step 3: Admin role, or a classic `repo` token).
10. **Build the app with the relay.** The app's relay client reads the URL at build time: `RAWTONIC_RELAY_URL=https://… npm run tauri -- build` (release workflows: a repository variable). A build without it has no contribution channels.

## Operating

- **Statistics** (only values counted at least 10 times in that week; the rest is one `(other)` line):
  ```sh
  curl -sS "$RELAY/stats/summary?week=2026-W43" -H "authorization: Bearer $ADMIN_TOKEN"
  ```
- **RAW samples** are not filed as issues. List them, newest first, and fetch one with what its donor announced:
  ```sh
  npx wrangler d1 execute rawtonic-relay --remote --command \
    "SELECT ref, objects, date(received_at / 1000, 'unixepoch') FROM submissions WHERE kind = 'raw-sample' AND state = 'filed' ORDER BY received_at DESC"
  npx wrangler r2 object get "rawtonic-contributions/raw-samples/<hash>/sample.json" --remote --pipe
  npx wrangler r2 object get "rawtonic-contributions/raw-samples/<hash>/<make>-<model>.<ext>" --remote --file sample.raw
  ```
  Keep downloaded samples out of git and out of shared folders (AGENTS.md: no photographs in the repository); they stay under the donor's licence (ADR 0017).
- **Retention** runs every 10 minutes (cron in `wrangler.jsonc`), in batches small enough for the Free plan's per-run limits: records and bucket objects of reports, crashes and camera tests go after 90 days and RAW samples after two years (~2,000 a day), and issues in the reports repository are deleted 90 days after they were created, oldest first (~2,800 a day) — so do not keep anything else in that repository. Check what is stored: `npx wrangler d1 execute rawtonic-relay --remote --command "SELECT kind, state, COUNT(*) FROM submissions GROUP BY kind, state"`.
- **Spam.** Set `POW_BITS` in `wrangler.jsonc` and deploy; requests without a proof of work with that many bits then get 403. The app always sends a 20-bit stamp (`crates/contribute`, `POW_BITS`), so any value up to 20 works with released apps; a higher one needs an app update first. Limits and global caps are in `services/relay/src/limits.ts`.
- **Secrets.** Rotate the GitHub token before it expires (`npx wrangler secret put GITHUB_TOKEN`). Changing `IP_HASH_KEY` only resets the current rate-limit windows.
- **Changes.** `npm test` and `npm run typecheck` in `services/relay` (also part of `scripts/check.sh`); a new migration goes into `migrations/` and is applied with the command from step 5 before deploying. A changed payload is a new `schema` number (ADR 0017).
- **Shutting down.** `npx wrangler delete`, then delete the D1 database and the R2 bucket in the dashboard, and the reports repository on GitHub; remove the URL from the app's build.
