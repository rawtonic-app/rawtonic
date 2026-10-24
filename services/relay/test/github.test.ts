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

  it("finds issues by the reference in their title only", async () => {
    let asked = "";
    const fetcher = async (url: string) => {
      asked = decodeURIComponent(url);
      return Response.json({
        items: [
          { title: "Crash 0123abcd · Rawtonic 1 · macos", node_id: "I_1" },
          { title: "Crash 0123abcde · other", node_id: "I_2" },
        ],
      });
    };
    expect(await new GitHubTracker("t", "a/b", fetcher).findByRef("0123abcd")).toEqual(["I_1"]);
    expect(asked).toContain("repo:a/b is:issue in:title 0123abcd");
    expect(await new GitHubTracker("t", "a/b", fetcher).findByRef("not a ref")).toEqual([]);
  });
});
