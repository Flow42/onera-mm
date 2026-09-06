<script lang="ts">
  import { commands } from '$lib/bridge';
  import { toCard, type ModCard } from '$lib/mod-card';
  import type { InstalledMod, LocalGame } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let selected = $state<string | null>(null);
  let mods = $state<InstalledMod[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let checking = $state(false);

  /** Artwork by mod id, fetched lazily so the list paints before the images. */
  let artwork = $state<Record<string, string>>({});

  const cards = $derived(mods.map(toCard));

  async function load(gameId: string) {
    selected = gameId;
    error = null;
    loading = true;
    try {
      mods = await commands.installedMods(gameId);
      void loadArtwork(mods);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /**
   * Fetch each mod's artwork.
   *
   * One request at a time, and never blocking the list: the images are decorat-
   * ion, and a slow or missing one must not delay the information the user came
   * for. The backend caches them, so this is a no-op after the first visit.
   */
  async function loadArtwork(rows: InstalledMod[]) {
    for (const mod of rows) {
      if (artwork[mod.mod_id] !== undefined) continue;
      try {
        const uri = await commands.modArtwork(mod.mod_id);
        if (uri !== null) artwork = { ...artwork, [mod.mod_id]: uri };
      } catch {
        // A mod without a picture is an ordinary mod.
      }
    }
  }

  /** Ask the provider which of these mods have a newer release. */
  async function checkUpdates() {
    if (selected === null) return;
    checking = true;
    error = null;
    try {
      mods = await commands.checkUpdates(selected);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      checking = false;
    }
  }

  /** Mods are added from the browser, so this only opens the right page. */
  async function openModPage() {
    const game = games.find((candidate) => candidate.id === selected);
    if (game === undefined) return;
    try {
      await commands.openModPage(game.adapter_id);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function openOnNexus(card: ModCard) {
    try {
      await commands.openNexusMod(card.gameSlug, card.providerModId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function remove(card: ModCard) {
    if (selected === null) return;
    // The preview is shown first because removal restores previous providers and
    // unmanaged originals; the user should see that before it happens.
    const preview = await commands.previewRemoval(selected, card.installationId);
    const summary =
      `${preview.deleted.length} deleted, ${preview.restored.length} restored, ` +
      `${preview.kept_shared.length} kept (shared by another mod)`;
    if (!window.confirm(`Remove ${card.name}?\n\n${summary}`)) return;
    await commands.remove(selected, card.installationId, false);
    await load(selected);
  }

  onMount(async () => {
    try {
      games = await commands.localGames();
      if (games[0] !== undefined) await load(games[0].id);
      else loading = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      loading = false;
    }
  });
</script>

<h1>Installed mods</h1>
<p class="toolbar">
  <button
    onclick={() => selected !== null && openModPage()}
    disabled={selected === null}
    title="Opens the mod listing on Nexus; the browser extension sends a mod back here"
  >
    Get mods
  </button>
  <button onclick={checkUpdates} disabled={selected === null || checking}>
    {checking ? 'Checking…' : 'Check for updates'}
  </button>
</p>
{#if error !== null}
  <p class="error" role="alert">{error}</p>
  {#if selected !== null}<button onclick={() => selected !== null && load(selected)}>Retry</button
    >{/if}
{/if}

<p class="toolbar">
  {#each games as game (game.id)}
    <button onclick={() => load(game.id)} disabled={selected === game.id}>{game.adapter_id}</button>
  {/each}
</p>

{#if loading}
  <p class="muted">Loading installed mods…</p>
{:else}
  <ul class="cards">
    {#each cards as card (card.installationId)}
      <li class="card" class:update={card.updateAvailable}>
        <div class="icon">
          {#if artwork[card.modId] !== undefined}
            <img src={artwork[card.modId]} alt="" />
          {:else}
            <span aria-hidden="true">{card.initials}</span>
          {/if}
        </div>

        <div class="detail">
          <h2>{card.name}</h2>
          <p class="meta">
            <!-- The version is whatever the author published, shown verbatim. -->
            <span class="tag">{card.version}</span>
            {#if card.author !== null}<span class="muted">by {card.author}</span>{/if}
            {#if card.updateAvailable}
              <span class="tag new">{card.latestVersion} available</span>
            {/if}
          </p>
          <p class="dates muted">
            <span>Installed {card.installed}</span>
            {#if card.latestPublished !== null}
              <span>Latest version {card.latestPublished}</span>
            {/if}
          </p>
        </div>

        <div class="actions">
          <button onclick={() => openOnNexus(card)} disabled={!card.canOpenPage}>
            Open on Nexus
          </button>
          <a href={`/verify?game=${selected}&installation=${card.installationId}`}>Verify</a>
          <a href={`/ownership?game=${selected}`}>Ownership</a>
          <button class="danger" onclick={() => remove(card)}>Remove</button>
        </div>
      </li>
    {/each}
  </ul>
{/if}
{#if !loading && error === null && mods.length === 0}<p class="muted">
    No mods installed for this game yet.
  </p>{/if}

<style>
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .cards {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  /* Each mod is its own raised surface rather than a table row: a card carries
     an image, two dates and a version tag, and a row would make all four
     compete for the same line. */
  .card {
    display: flex;
    gap: 1rem;
    align-items: center;
    padding: 0.75rem;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: 0 1px 3px #0006;
  }
  .card.update {
    border-color: var(--accent);
  }
  .icon {
    flex: none;
    width: 72px;
    height: 72px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--bg);
    border: 1px solid var(--line);
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-weight: 600;
    font-size: 1.1rem;
  }
  .icon img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .detail {
    flex: 1;
    min-width: 0;
  }
  .detail h2 {
    font-size: 1rem;
    margin: 0 0 0.25rem;
    /* A mod name can be very long; it must not push the actions off the card. */
    overflow-wrap: anywhere;
  }
  .meta,
  .dates {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 0.75rem;
    align-items: center;
    margin: 0.2rem 0 0;
  }
  .tag {
    padding: 0.1rem 0.45rem;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--bg);
    font-variant-numeric: tabular-nums;
  }
  .tag.new {
    border-color: var(--accent);
    color: var(--accent);
  }
  .actions {
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.35rem;
    text-align: center;
  }
  .actions a {
    color: var(--accent);
    font-size: 0.9rem;
  }
</style>
