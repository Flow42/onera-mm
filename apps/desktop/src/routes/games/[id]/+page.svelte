<!--
  One game's submenu.

  A level down from the games list: it names the installation and offers the
  sections that are scoped to it. Every entry carries the game with it, so a
  section opened from here is never about a different installation.
-->
<script lang="ts">
  import { page } from '$app/state';
  import CardList from '$lib/components/CardList.svelte';
  import ViewToggle from '$lib/components/ViewToggle.svelte';
  import { commands } from '$lib/bridge';
  import { gameIcon } from '$lib/game-icons.svelte';
  import { gameLabel, gameSections, initialsOf } from '$lib/navigation';
  import type { DownloadPaths, GamePaths, InstalledMod, LocalGame } from '$lib/types';
  import { viewMode } from '$lib/view-mode.svelte';
  import { onMount } from 'svelte';

  const gameId = $derived(page.params.id ?? '');

  let games = $state<LocalGame[]>([]);
  let mods = $state<InstalledMod[] | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  /** Where the game lives and where its archives are extracted. */
  let paths = $state<GamePaths | null>(null);
  /** Where this game's downloads are kept, and which setting decided that. */
  let downloads = $state<DownloadPaths | null>(null);
  /** What the last change did, so the move can be reported rather than guessed. */
  let downloadNotice = $state<string | null>(null);
  let downloadError = $state<string | null>(null);
  let changingDownloads = $state(false);

  const layout = viewMode('game-sections', 'tiles');

  const game = $derived(games.find((candidate) => candidate.id === gameId) ?? null);
  const sections = $derived(gameSections(gameId));

  onMount(async () => {
    try {
      games = await commands.localGames();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
    void loadPaths();
    try {
      // A count for the Mods card, from the cheap read model. It never contacts
      // the provider, so it cannot say anything about updates and does not try.
      mods = await commands.installedMods(gameId);
    } catch {
      // Left null: "not counted" is not the same as "none installed".
      mods = null;
    }
  });

  async function loadPaths() {
    try {
      [paths, downloads] = await Promise.all([
        commands.gamePaths(gameId),
        commands.downloadPaths(gameId),
      ]);
    } catch {
      // The page is still worth showing without them: they are detail about a
      // game whose sections are the reason to be here.
      paths = null;
      downloads = null;
    }
  }

  /** How a move reads once it has happened. */
  function movedText(change: { moved: number; root: string; previous: string }): string {
    if (change.moved === 0) {
      return `Downloads for this game now go to ${change.root}.`;
    }
    const what = change.moved === 1 ? '1 archive was' : `${change.moved} archives were`;
    return `Downloads for this game now go to ${change.root}. ${what} moved there from ${change.previous}.`;
  }

  /**
   * Give this game a download directory of its own.
   *
   * The picker, the checks and the move are one backend call, so what comes
   * back is either a completed change or the reason it was refused — never a
   * setting pointing somewhere the archives did not reach.
   */
  async function chooseDownloads() {
    changingDownloads = true;
    downloadError = null;
    downloadNotice = null;
    try {
      const change = await commands.pickDownloadRoot(gameId);
      if (change === null) return; // Cancelled: nothing to say.
      downloadNotice = movedText(change);
      await loadPaths();
    } catch (e) {
      downloadError = e instanceof Error ? e.message : String(e);
    } finally {
      changingDownloads = false;
    }
  }

  /** Put this game back on the directory every other game uses. */
  async function resetDownloads() {
    changingDownloads = true;
    downloadError = null;
    downloadNotice = null;
    try {
      const change = await commands.resetDownloadRoot(gameId);
      downloadNotice = movedText(change);
      await loadPaths();
    } catch (e) {
      downloadError = e instanceof Error ? e.message : String(e);
    } finally {
      changingDownloads = false;
    }
  }

  /** The one-line count under a section's name, where there is one to give. */
  function detail(key: string): string | null {
    if (key !== 'mods') return null;
    if (mods === null) return null;
    return mods.length === 1 ? '1 mod installed' : `${mods.length} mods installed`;
  }
</script>

{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

<header class="page-heading">
  <div class="identity">
    <!-- The game's own icon, found in its installation directory. Plenty of
         games ship none, which is why the initials stay as the fallback. -->
    <span class="game-icon" aria-hidden="true">
      {#if gameIcon(gameId) !== null}
        <img src={gameIcon(gameId)} alt="" />
      {:else}
        {initialsOf(game === null ? '?' : gameLabel(game.install_root))}
      {/if}
    </span>
    <div>
      <p class="crumb"><a href="/games">← All games</a></p>
      <h1>{game === null ? 'Game' : gameLabel(game.install_root)}</h1>
      {#if game !== null}
        <p class="lede">
          <span class="tag">{game.adapter_id}</span>
        </p>
      {:else if !loading}
        <p class="lede">
          This installation is not in the registered list. It may have been removed — open the games
          list and confirm it again.
        </p>
      {/if}
    </div>
  </div>
  <ViewToggle mode={layout.current} onchange={(mode) => layout.set(mode)} label="Sections layout" />
</header>

{#if paths !== null}
  <!-- The two directories that matter for this game: where it is, and where
       what Onera downloads for it is kept. -->
  <div class="panel locations">
    <div class="location">
      <p class="location-label">Installation</p>
      <p class="path">{paths.install_root}</p>
    </div>

    {#if downloads !== null}
      <div class="location">
        <p class="location-label">
          Downloads
          {#if downloads.scope === 'game'}
            <span class="tag new">this game</span>
          {:else}
            <span class="tag">{downloads.scope === 'global' ? 'shared' : "Onera's own"}</span>
          {/if}
          {#if downloads.archives > 0}
            <span class="tag">{downloads.archives} archives</span>
          {/if}
        </p>
        <p class="path">{downloads.root}</p>
        <p class="toolbar">
          <button onclick={chooseDownloads} disabled={changingDownloads}>
            {changingDownloads ? 'Working…' : 'Change for this game…'}
          </button>
          {#if downloads.scope === 'game'}
            <button class="ghost" onclick={resetDownloads} disabled={changingDownloads}>
              Use the shared directory
            </button>
          {/if}
        </p>
        <p class="muted note">
          {#if downloads.scope === 'game'}
            This game keeps its downloaded mods here, instead of the shared directory (<span
              class="path">{downloads.inherited}</span
            >). Changing it moves the archives Onera already has for this game; the database follows
            them, so nothing needs re-downloading.
          {:else}
            This game uses the directory every game uses, which you can change in
            <a href="/settings">Settings</a>. Giving it one of its own is worth doing when the game
            lives on another disk — the download lands on the same filesystem it will be installed
            from. Either way, the archives Onera already has are moved rather than re-downloaded.
          {/if}
        </p>
        {#if downloadNotice !== null}<p class="notice">{downloadNotice}</p>{/if}
        {#if downloadError !== null}<p class="error" role="alert">{downloadError}</p>{/if}
      </div>
    {/if}
  </div>
{/if}

<CardList mode={layout.current} label="Sections">
  {#each sections as section (section.key)}
    <li class="card actionable">
      <span class="icon" aria-hidden="true">{section.glyph}</span>
      <div class="body">
        <h2 class="title"><a class="stretch" href={section.href}>{section.label}</a></h2>
        {#if detail(section.key) !== null}
          <p class="meta"><span class="tag">{detail(section.key)}</span></p>
        {/if}
        <p class="muted description">{section.description}</p>
      </div>
      <span class="chevron above tile-hide" aria-hidden="true">›</span>
    </li>
  {/each}
</CardList>

<style>
  .crumb {
    margin: 0 0 0.35rem;
    font-size: 0.85rem;
  }
  .crumb a {
    color: var(--muted);
    text-decoration: none;
  }
  .crumb a:hover {
    color: var(--text);
  }
  .lede {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem 0.75rem;
  }
  .chevron {
    color: var(--faint);
    font-size: 1.4rem;
    line-height: 1;
  }
  .identity {
    display: flex;
    align-items: flex-start;
    gap: 0.9rem;
  }
  .game-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 56px;
    height: 56px;
    margin-top: 1.15rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    overflow: hidden;
    font-size: 1.15rem;
    font-weight: 700;
    color: var(--faint);
  }
  .game-icon img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .locations {
    display: grid;
    gap: 1rem;
    margin-bottom: 1.25rem;
  }
  .location-label {
    margin: 0 0 0.2rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
  }
  .location .path {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .note {
    margin: 0.4rem 0 0;
    max-width: 68ch;
    font-size: 0.85rem;
  }
  .notice {
    margin: 0.5rem 0 0;
    color: var(--accent-strong);
    font-size: 0.9rem;
  }
  .description {
    margin: 0.3rem 0 0;
    font-size: 0.88rem;
  }
  /* A section's mark is a label rather than artwork, so it stays a small badge
     instead of growing to fill the square the way a mod's picture does. */
  .card .icon {
    background: var(--accent-soft);
    color: var(--accent-strong);
    border-color: var(--accent-line);
  }
  :global(.cards.tiles) .card {
    justify-content: flex-start;
  }
  :global(.cards.tiles) .card .icon {
    flex: none;
    width: 40px;
    height: 40px;
    font-size: 1.1rem;
  }
</style>
