// SPDX-License-Identifier: AGPL-3.0-or-later
// Issues in the private repository (ADR 0017), filed by the bot token.

export interface NewIssue {
  title: string;
  body: string;
  labels: string[];
}
export interface FiledIssue {
  number: number;
  nodeId: string;
}

/** Where submissions are filed; the relay only creates and deletes. */
export interface IssueTracker {
  create(issue: NewIssue): Promise<FiledIssue>;
  /** Deletes the issue for good (edit history included). Resolves when it
   * is gone, also when it was already deleted. */
  delete(nodeId: string): Promise<void>;
  /** The oldest issues (any state), oldest first: retention reads the
   * repository itself, so an issue whose filing got no answer (never
   * recorded) still goes after 90 days. */
  listOldest(count: number): Promise<{ nodeId: string; createdAt: number }[]>;
}

export class GitHubError extends Error {}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export class GitHubTracker implements IssueTracker {
  constructor(
    private readonly token: string,
    private readonly repo: string,
    private readonly fetcher: Fetch = (input, init) => fetch(input, init),
    private readonly api = "https://api.github.com",
  ) {
    if (!/^[\w.-]+\/[\w.-]+$/.test(repo)) throw new GitHubError("GITHUB_REPO must be owner/name");
  }

  private headers() {
    return {
      authorization: `Bearer ${this.token}`,
      accept: "application/vnd.github+json",
      "content-type": "application/json",
      "user-agent": "rawtonic-relay",
      "x-github-api-version": "2022-11-28",
    };
  }

  async create(issue: NewIssue): Promise<FiledIssue> {
    const response = await this.fetcher(`${this.api}/repos/${this.repo}/issues`, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify(issue),
      signal: AbortSignal.timeout(10_000),
    });
    if (response.status !== 201) throw new GitHubError(`create issue: HTTP ${response.status}`);
    const created = (await response.json()) as { number?: unknown; node_id?: unknown };
    if (typeof created.number !== "number" || typeof created.node_id !== "string") {
      throw new GitHubError("create issue: unexpected response");
    }
    return { number: created.number, nodeId: created.node_id };
  }

  async listOldest(count: number): Promise<{ nodeId: string; createdAt: number }[]> {
    const response = await this.fetcher(
      `${this.api}/repos/${this.repo}/issues?state=all&sort=created&direction=asc&per_page=${count}`,
      { method: "GET", headers: this.headers(), signal: AbortSignal.timeout(10_000) },
    );
    if (response.status !== 200) throw new GitHubError(`list issues: HTTP ${response.status}`);
    const items = (await response.json()) as { node_id?: unknown; created_at?: unknown; pull_request?: unknown }[];
    if (!Array.isArray(items)) throw new GitHubError("list issues: unexpected response");
    return items
      .filter((i) => !i.pull_request && typeof i.node_id === "string" && typeof i.created_at === "string")
      .map((i) => ({ nodeId: i.node_id as string, createdAt: Date.parse(i.created_at as string) }))
      .filter((i) => Number.isFinite(i.createdAt));
  }

  async delete(nodeId: string): Promise<void> {
    const response = await this.fetcher(`${this.api}/graphql`, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({
        query: "mutation($id: ID!) { deleteIssue(input: { issueId: $id }) { clientMutationId } }",
        variables: { id: nodeId },
      }),
      signal: AbortSignal.timeout(10_000),
    });
    if (response.status !== 200) throw new GitHubError(`delete issue: HTTP ${response.status}`);
    const result = (await response.json()) as { errors?: { type?: string }[] };
    if (result.errors?.length) {
      if (result.errors.every((e) => e.type === "NOT_FOUND")) return;
      throw new GitHubError(`delete issue: ${result.errors.map((e) => e.type ?? "error").join(", ")}`);
    }
  }
}
