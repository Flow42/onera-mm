//! Game adapters.
//!
//! An adapter is the only place that knows anything about a particular game. It
//! answers four questions: is this directory really the game, where does content
//! get deployed, how does an archive's contents map onto those roots, and is a
//! given target legal.
//!
//! Adding a game means adding one module here and registering it in
//! [`all_adapters`] — no change to the installer, the planner or any provider
//! client. See `docs/game-adapter-guide.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cyberpunk2077;

pub use cyberpunk2077::Cyberpunk2077;
use onera_core::ports::GameAdapter;

/// Every adapter this build ships.
#[must_use]
pub fn all_adapters() -> Vec<&'static dyn GameAdapter> {
    vec![&Cyberpunk2077]
}

/// Find an adapter by its slug.
#[must_use]
pub fn adapter_by_id(id: &str) -> Option<&'static dyn GameAdapter> {
    all_adapters().into_iter().find(|a| a.id() == id)
}

/// Find an adapter that claims a provider's game slug.
#[must_use]
pub fn adapter_for_provider_slug(slug: &str) -> Option<&'static dyn GameAdapter> {
    all_adapters()
        .into_iter()
        .find(|a| a.provider_slugs().contains(&slug))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapters_have_unique_ids_and_claims() {
        let adapters = all_adapters();
        let mut ids: Vec<_> = adapters.iter().map(|a| a.id()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "two adapters share an id");

        let mut app_ids: Vec<u32> = adapters
            .iter()
            .flat_map(|a| a.steam_app_ids())
            .copied()
            .collect();
        app_ids.sort_unstable();
        let before = app_ids.len();
        app_ids.dedup();
        assert_eq!(
            app_ids.len(),
            before,
            "two adapters claim the same Steam app"
        );
    }

    #[test]
    fn adapters_are_findable_by_id_and_provider_slug() {
        assert_eq!(
            adapter_by_id("cyberpunk2077").unwrap().id(),
            "cyberpunk2077"
        );
        assert_eq!(
            adapter_for_provider_slug("cyberpunk2077")
                .unwrap()
                .display_name(),
            "Cyberpunk 2077"
        );
        assert!(adapter_by_id("no-such-game").is_none());
        assert!(adapter_for_provider_slug("skyrimspecialedition").is_none());
    }
}
