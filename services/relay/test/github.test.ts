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

  it("finds issues by their marker in the full issue list, and refuses to guess", async () => {
    const asked: string[] = [];
    const page = (n: number) =>
      Array.from({ length: n }, (_, i) => ({ body: `x <!-- rawtonic-relay:${i === 3 ? "aa" : "bb"} -->`, node_id: `I_${n}_${i}` }));
    const fetcher = async (url: string) => {
      asked.push(url);
      return Response.json(asked.length === 1 ? page(100) : [...page(5), { body: "rawtonic-relay:aa", node_id: "PR", pull_request: {} }]);
    };
    expect(await new GitHubTracker("t", "a/b", fetcher).findByMarker("rawtonic-relay:aa", 0)).toEqual(["I_100_3", "I_5_3"]);
    expect(asked[0]).toContain("/repos/a/b/issues?state=all&since=1970-01-01T00%3A00%3A00.000Z&per_page=100&page=1");
    expect(asked[1]).toContain("page=2");
    const failing = new GitHubTracker("t", "a/b", async () => new Response("", { status: 502 }));
    await expect(failing.findByMarker("m", 0)).rejects.toThrow("HTTP 502");
    const endless = new GitHubTracker("t", "a/b", async () => Response.json(page(100)));
    await expect(endless.findByMarker("m", 0)).rejects.toThrow("too many");
  });
});
