// SPDX-License-Identifier: AGPL-3.0-or-later
import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest(async () => ({
      wrangler: { configPath: "./wrangler.jsonc" },
      miniflare: {
        bindings: {
          TEST_MIGRATIONS: await readD1Migrations("./migrations"),
          GITHUB_TOKEN: "test-token",
          IP_HASH_KEY: "test-ip-key",
          ADMIN_TOKEN: "test-admin",
        },
      },
    })),
  ],
});
