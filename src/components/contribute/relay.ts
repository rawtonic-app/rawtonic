// SPDX-License-Identifier: AGPL-3.0-or-later
// The contribution channels' calls into the app (ADR 0017,
// src-tauri/src/contribute.rs). Nothing here runs on its own: a channel
// prepares a body, the user reviews it in ReviewSendDialog, and only the
// dialog's Send button sends it.
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";

export type ContributionChannel = "report" | "crash" | "cameraTest" | "rawSample" | "stats";

/** A scrubbed body exactly as it will be sent, with its SHA-256. */
export interface PreparedContribution {
  channel: ContributionChannel;
  body: string;
  digest: string;
}

export interface ContributionReceipt {
  /** Deletes the submission; shown to the user. */
  id?: string | null;
  ref?: string | null;
  uploadUrl?: string | null;
  expiresAt?: string | null;
}

/** Error codes the send and delete commands answer with. */
export type ContributionError =
  | "not-configured"
  | "unapproved"
  | "rate-limited"
  | "not-found"
  | "refused"
  | "network";

export const contributionAvailable = () => invoke<boolean>(Invokes.ContributionAvailable);

export const prepareContribution = (channel: ContributionChannel, payload: Record<string, unknown>) =>
  invoke<PreparedContribution>(Invokes.ContributionPrepare, { channel, payload });

export const sendContribution = (prepared: PreparedContribution) =>
  invoke<ContributionReceipt>(Invokes.ContributionSend, { prepared });

export const deleteContribution = (id: string) => invoke<void>(Invokes.ContributionDelete, { id });

/** The body as the dialog shows it: verbatim, except that a screenshot's
 * base64 data is named instead of printed (the image is shown below). */
export const reviewView = (body: string): { text: string; screenshot?: { src: string; bytes: number } } => {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return { text: body };
  }
  const shot = (parsed as { screenshot?: { mime?: unknown; data?: unknown } } | null)?.screenshot;
  if (!shot || typeof shot.data !== "string" || typeof shot.mime !== "string") return { text: body };
  const quoted = JSON.stringify(shot.data);
  const bytes = Math.floor((shot.data.length * 3) / 4) - (shot.data.match(/=+$/)?.[0].length ?? 0);
  return {
    text: body.replace(quoted, `"‹image, shown below›"`),
    screenshot: { src: `data:${shot.mime};base64,${shot.data}`, bytes },
  };
};
