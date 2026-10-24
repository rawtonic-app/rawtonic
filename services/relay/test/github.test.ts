// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { GitHubTracker } from "../src/github";

describe("GitHub tracker", () => {
  it("creates issues in the configured repository and deletes them by node id", async () => {
    const calls: { url: string; body: unknown; auth: string | null }[] = [];
    const fetcher = async (url: string, init: RequestInit) => {
      calls.push({ url, body: JSON.parse(init.body as string), auth: new Headers(init.headers).get("authorization") });
      if (url.endsWith("/issues")) return Response.json({ number: 7, node_id: "I_7" }, { status: 201 });
      return Response.json({ data: { deleteIssue: { clientMutationId: null } } });
    };
    const tracker = new GitHubTracker("tok", "rawtonic-app/reports", fetcher);
    expect(await tracker.create({ title: "t", body: "b", labels: ["report"] })).toEqual({ number: 7, nodeId: "I_7" });
    await tracker.delete("I_7");
    expect(calls[0].url).toBe("https://api.github.com/repos/rawtonic-app/reports/issues");
    expect(calls[0].auth).toBe("Bearer tok");
    expect(calls[1].url).toBe("https://api.github.com/graphql");
    expect(calls[1].body).toMatchObject({ variables: { id: "I_7" } });
  });

  it("treats an issue already gone as deleted and other errors as failures", async () => {
    const answer = (errors: unknown) => async () => Response.json({ errors });
    await new GitHubTracker("t", "a/b", answer([{ type: "NOT_FOUND" }])).delete("I_1");
    await expect(new GitHubTracker("t", "a/b", answer([{ type: "FORBIDDEN" }])).delete("I_1")).rejects.toThrow("FORBIDDEN");
    await expect(
      new GitHubTracker("t", "a/b", async () => new Response("", { status: 500 })).create({ title: "", body: "", labels: [] }),
    ).rejects.toThrow("HTTP 500");
  });

  it("lists the oldest issues, oldest first, without pull requests", async () => {
    let asked = "";
    const fetcher = async (url: string) => {
      asked = url;
      return Response.json([
        { node_id: "I_1", created_at: "2026-01-01T00:00:00Z" },
        { node_id: "PR_2", created_at: "2026-01-02T00:00:00Z", pull_request: {} },
        { node_id: "I_3", created_at: "2026-01-03T00:00:00Z" },
      ]);
    };
    expect(await new GitHubTracker("t", "a/b", fetcher).listOldest(20)).toEqual([
      { nodeId: "I_1", createdAt: Date.UTC(2026, 0, 1) },
      { nodeId: "I_3", createdAt: Date.UTC(2026, 0, 3) },
    ]);
    expect(asked).toBe("https://api.github.com/repos/a/b/issues?state=all&sort=created&direction=asc&per_page=20");
    const failing = new GitHubTracker("t", "a/b", async () => new Response("", { status: 401 }));
    await expect(failing.listOldest(20)).rejects.toThrow("HTTP 401");
  });
});
