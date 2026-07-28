//! Domain entities.
//!
//! Everything here is provider- and game-agnostic. Provider-specific data is
//! carried in opaque identifier newtypes and in an untyped `metadata` JSON blob
//! that only the originating provider adapter interprets.

pub mod archive;
pub mod game;
pub mod operation;
pub mod provider_stack;
pub mod release;
