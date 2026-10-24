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
  /** Node ids of the issues whose title carries this reference (to clean
   * up a filing whose issue id was never recorded). */
  findByRef(ref: string): Promise<string[]>;
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

  async findByRef(ref: string): Promise<string[]> {
    if (!/^[0-9a-f]{8}$/.test(ref)) return [];
    const q = encodeURIComponent(`repo:${this.repo} is:issue in:title ${ref}`);
    const response = await this.fetcher(`${this.api}/search/issues?q=${q}&per_page=10`, {
      method: "GET",
      headers: this.headers(),
      signal: AbortSignal.timeout(10_000),
    });
    if (response.status !== 200) throw new GitHubError(`search issues: HTTP ${response.status}`);
    const found = (await response.json()) as { items?: { title?: unknown; node_id?: unknown }[] };
    return (found.items ?? [])
      .filter((i) => typeof i.title === "string" && i.title.split(" ").includes(ref) && typeof i.node_id === "string")
      .map((i) => i.node_id as string);
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
