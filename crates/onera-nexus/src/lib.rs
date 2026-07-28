//! Nexus Mods API client.
//!
//! Implements [`onera_core::ports::ModProvider`] and
//! [`onera_core::ports::AuthProvider`] against the Nexus Mods API. Nothing
//! outside this crate knows what a "game domain" or a "mod file version" is.
//!
//! ## What this client assumes
//!
//! Onera targets API **v3** (`https://api.nexusmods.com/v3`). The v3
//! specification it was written against covers mods, mod files and mod file
//! versions, but not credential validation, the supported-game catalogue or
//! download resolution. Those three calls use the documented v1 endpoints and
//! are confined to clearly marked places in [`client`] and [`auth`], so
//! migrating them is a change to two functions.
//!
//! Every assumption, and what breaks if Nexus changes it, is written up in
//! `docs/nexus-api-assumptions.md`.
//!
//! ## What this client will not do
//!
//! It does not scrape mod pages. Everything Onera shows comes from the API. The
//! browser extension extracts only a game domain and a mod id from the URL and
//! hands them here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod auth;
pub mod client;
pub mod error;
pub mod models;
pub mod retry;

pub use auth::{ApiKeyAuth, SECRET_KEY};
pub use client::{NexusClient, NexusConfig, DEFAULT_V1_BASE, DEFAULT_V3_BASE};
pub use retry::{RateLimit, RetryPolicy};
