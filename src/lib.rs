//! Duskr - native system-wide speech-to-text for Linux.
//!
//! The crate is split so that no layer depends on another's implementation:
//! `core` holds configuration and observable state, `audio` and `input` own the
//! hardware, `asr` is a replaceable backend registry, `daemon` orchestrates,
//! and `ipc`/`cli`/`integrations` are all clients of the daemon's state.

pub mod asr;
pub mod audio;
pub mod cli;
pub mod core;
pub mod daemon;
pub mod input;
pub mod integrations;
pub mod ipc;
pub mod migrate;
pub mod models;
pub mod text;
