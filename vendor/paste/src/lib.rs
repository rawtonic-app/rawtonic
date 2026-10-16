//! `paste`'s macros, provided by its maintained drop-in successor `pastey`
//! (MIT OR Apache-2.0, <https://github.com/as1100k/pastey>). Crates that use
//! `paste::paste!` (or the deprecated `item!`/`expr!`) build unchanged.

#![no_std]

pub use pastey::{expr, item, paste};
