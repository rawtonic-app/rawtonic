// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contribution channels' shared core (ADR 0017): scrubbing, the body
//! the user reviews and the relay client. Nothing here sends on its own;
//! a channel prepares a body, the user reviews it, and only then is it sent.

pub mod client;
pub mod payload;
pub mod scrub;
