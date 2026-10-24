// SPDX-License-Identifier: AGPL-3.0-or-later
// The contribution channels' calls into the app (ADR 0017,
// src-tauri/src/contribute.rs). Nothing here runs on its own: a channel
// prepares a body, the user reviews it in ReviewSendDialog, and only the
// dialog's Send button sends it.
import { invoke } from "@tauri-apps/api/core";
import { Invokes } from "../ui/AppProperties";

export type ContributionChannel = "report" | "crash" | "cameraTest" | "stats";

/** A file sent after the body, as it is; the body names its size and
 * SHA-256. */
export interface ContributionAttachment {
  slot: "screenshot" | "photo";
  size: number;
  sha256: string;
  /** A screenshot's bytes, base64. */
  data?: string;
  /** The photo's file on this computer (not sent, only its bytes). */
  path?: string;
}

/** A scrubbed body exactly as it will be sent, with its SHA-256, and the
 * files that follow it. */
export interface PreparedContribution {
  channel: ContributionChannel;
  body: string;
  digest: string;
  attachments?: ContributionAttachment[];
}

export interface ContributionReceipt {
  /** Deletes the submission with its files; shown to the user. */
  id?: string | null;
  ref?: string | null;
  /** Announced files whose upload was not confirmed (one may still have
   * arrived; it goes with the report). */
  notUploaded?: ContributionAttachment["slot"][];
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

/** The files the dialog shows below the body: the screenshot as an image,
 * the photo's file by its size. */
export const attachmentsView = (
  prepared: PreparedContribution,
): { screenshot?: { src: string; bytes: number }; photo?: { bytes: number } } => {
  const view: ReturnType<typeof attachmentsView> = {};
  for (const a of prepared.attachments ?? []) {
    if (a.slot === "photo") view.photo = { bytes: a.size };
    if (a.slot === "screenshot" && a.data) {
      let mime = "image/png";
      try {
        const announced = (JSON.parse(prepared.body) as { screenshot?: { mime?: unknown } }).screenshot?.mime;
        if (announced === "image/jpeg") mime = announced;
      } catch {
        // The body is shown as it is either way.
      }
      view.screenshot = { src: `data:${mime};base64,${a.data}`, bytes: a.size };
    }
  }
  return view;
};
