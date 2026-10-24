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
  /** Node ids of the issues (any state) updated since `since` whose body
   * carries `marker`. Reads the issue list, which is consistent, not the
   * search index; throws when it cannot read all of it. */
  findByMarker(marker: string, since: number, maxPages: number): Promise<string[]>;
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

  async findByMarker(marker: string, since: number, maxPages = 10): Promise<string[]> {
    const found: string[] = [];
    const after = new Date(since).toISOString();
    for (let page = 1; page <= maxPages; page++) {
      const response = await this.fetcher(
        `${this.api}/repos/${this.repo}/issues?state=all&since=${encodeURIComponent(after)}&per_page=100&page=${page}`,
        { method: "GET", headers: this.headers(), signal: AbortSignal.timeout(10_000) },
      );
      if (response.status !== 200) throw new GitHubError(`list issues: HTTP ${response.status}`);
      const items = (await response.json()) as { body?: unknown; node_id?: unknown; pull_request?: unknown }[];
      if (!Array.isArray(items)) throw new GitHubError("list issues: unexpected response");
      for (const item of items) {
        if (!item.pull_request && typeof item.body === "string" && item.body.includes(marker) && typeof item.node_id === "string") {
          found.push(item.node_id);
        }
      }
      if (items.length < 100) return found;
    }
    throw new GitHubError("list issues: too many to read");
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
