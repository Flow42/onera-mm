<!--
  The mods installed for one game.

  Reached from the game's submenu, so the installation is normally carried in
  the address; opened directly it falls back to the first registered game
  rather than showing nothing.
-->
<script lang="ts">
  import { page } from '$app/state';
  import CardList from '$lib/components/CardList.svelte';
  import ViewToggle from '$lib/components/ViewToggle.svelte';
  import { commands } from '$lib/bridge';
  import { foldIntoTree, formatSize, remainderNote } from '$lib/file-tree';
  import { toCard, type ModCard } from '$lib/mod-card';
  import { gameLabel } from '$lib/navigation';
  import type { InstalledMod, LocalGame, ModContents } from '$lib/types';
  import { viewMode } from '$lib/view-mode.svelte';
  import { onMount } from 'svelte';
  import { slide } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';

  let games = $state<LocalGame[]>([]);
  let selected = $state<string | null>(null);
  let mods = $state<InstalledMod[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let checking = $state(false);

  /** Artwork by mod id, fetched lazily so the list paints before the images. */
  let artwork = $state<Record<string, string>>({});

  /**
   * How many files one expanded mod lists.
   *
   * The point of the expansion is to say what a mod put on disk, not to be a
   * file manager: ten lines answer that, and the button below opens the real
   * thing for anyone who wants the rest.
   */
  const FILE_LIMIT = 10;

  /** The mod whose files are showing, if any. One at a time. */
  let expanded = $state<string | null>(null);
  /** Files by installation id, kept so re-opening a card does not re-read. */
  let contents = $state<Record<string, ModContents>>({});
  let contentsError = $state<Record<string, string>>({});

  /**
   * Show or hide one mod's files.
   *
   * The read happens on the first expand rather than with the list: a page of
   * twenty mods would otherwise stat several hundred files to draw something
   * nobody has asked to see.
   */
  async function toggleFiles(card: ModCard) {
    if (expanded === card.installationId) {
      expanded = null;
      return;
    }
    expanded = card.installationId;
    if (selected === null || contents[card.installationId] !== undefined) return;
    try {
      contents[card.installationId] = await commands.modContents(
        selected,
        card.installationId,
        FILE_LIMIT,
      );
    } catch (e) {
      contentsError[card.installationId] = e instanceof Error ? e.message : String(e);
    }
  }

  /**
   * Whether a click on the card should open it.
   *
   * A card carries buttons and links of its own, and a click on one of those is
   * a click on that control — not on the card behind it.
   */
  function cardClicked(event: MouseEvent, card: ModCard) {
    const target = event.target;
    if (target instanceof Element && target.closest('a, button') !== null) return;
    void toggleFiles(card);
  }

  /** Open one mod's files in the system file manager. */
  async function browse(card: ModCard) {
    if (selected === null) return;
    try {
      await commands.browseModFiles(selected, card.installationId);
    } catch (e) {
      contentsError[card.installationId] = e instanceof Error ? e.message : String(e);
    }
  }

  const layout = viewMode('mods', 'list');
  const cards = $derived(mods.map(toCard));
  const game = $derived(games.find((candidate) => candidate.id === selected) ?? null);

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
    if (game === null) return;
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
      const requested = page.url.searchParams.get('game');
      const target = games.find((candidate) => candidate.id === requested) ?? games[0];
      if (target !== undefined) await load(target.id);
      else loading = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      loading = false;
    }
  });
</script>

<header class="page-heading">
  <div>
    {#if selected !== null}
      <p class="crumb">
        <a href={`/games/${encodeURIComponent(selected)}`}
          >← {game === null ? 'Game' : gameLabel(game.install_root)}</a
        >
      </p>
    {/if}
    <h1>Installed mods</h1>
    <p class="lede">
      What is deployed for this installation. A version is shown exactly as its author published it,
      and an update is only claimed after a check.
    </p>
  </div>
  <div class="toolbar">
    <ViewToggle mode={layout.current} onchange={(mode) => layout.set(mode)} label="Mods layout" />
    <button
      onclick={() => selected !== null && openModPage()}
      disabled={selected === null}
      title="Opens the mod listing on Nexus; the browser extension sends a mod back here"
    >
      Get mods
    </button>
    <button class="primary" onclick={checkUpdates} disabled={selected === null || checking}>
      {checking ? 'Checking…' : 'Check for updates'}
    </button>
  </div>
</header>

{#if error !== null}
  <p class="error" role="alert">{error}</p>
  {#if selected !== null}<button onclick={() => selected !== null && load(selected)}>Retry</button
    >{/if}
{/if}

{#if games.length > 1}
  <!-- More than one installation is registered, so the list says which one it
       is about and lets the user move between them without going back up. -->
  <p class="toolbar switcher">
    {#each games as candidate (candidate.id)}
      <button
        class:selected={selected === candidate.id}
        onclick={() => load(candidate.id)}
        disabled={selected === candidate.id}>{candidate.adapter_id}</button
      >
    {/each}
  </p>
{/if}

{#if loading}
  <p class="muted">Loading installed mods…</p>
{:else}
  <CardList mode={layout.current} label="Installed mods">
    {#each cards as card (card.installationId)}
      {@const open = expanded === card.installationId}
      {@const files = contents[card.installationId]}
      <!-- The card is the disclosure. The heading carries the real control, so
           the keyboard and a screen reader get a button with a state; the click
           handler is a convenience for the pointer and defers to any control it
           lands on. -->
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <li
        class="card"
        class:update={card.updateAvailable}
        class:expanded={open}
        onclick={(event) => cardClicked(event, card)}
      >
        <!-- One dot, at the left edge of the card: green while the installed
             version is the newest Onera knows of, accent once a newer one has
             actually been found. The glow is what makes it readable at this
             size without making the dot itself larger. -->
        <span class="status" class:new={card.updateAvailable} aria-hidden="true"></span>

        <div class="icon">
          {#if artwork[card.modId] !== undefined}
            <img src={artwork[card.modId]} alt="" />
          {:else}
            <span aria-hidden="true">{card.initials}</span>
          {/if}
        </div>

        <div class="body">
          <h2 class="title">
            <button
              class="disclosure"
              aria-expanded={open}
              aria-controls={`files-${card.installationId}`}
              onclick={() => toggleFiles(card)}
            >
              <span class="caret" class:open aria-hidden="true">›</span>{card.name}
            </button>
          </h2>
          <p class="meta">
            <!-- The version is whatever the author published, shown verbatim. -->
            <span class="tag">{card.version}</span>
            {#if card.author !== null}<span class="muted tile-hide">by {card.author}</span>{/if}
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
          <a
            class="link tile-hide"
            href={`/verify?game=${selected}&installation=${card.installationId}`}>Verify</a
          >
          <a class="link tile-hide" href={`/ownership?game=${selected}`}>Ownership</a>
          <button class="danger tile-hide" onclick={() => remove(card)}>Remove</button>
        </div>

        {#if open}
          <div
            class="files"
            id={`files-${card.installationId}`}
            transition:slide={{ duration: 160, easing: cubicOut }}
          >
            {#if contentsError[card.installationId] !== undefined}
              <p class="error" role="alert">{contentsError[card.installationId]}</p>
            {:else if files === undefined}
              <p class="muted">Reading this mod's files…</p>
            {:else if files.entries.length === 0}
              <p class="muted">
                {files.kind === 'archive'
                  ? 'Nothing is deployed into the game; the downloaded archive is all there is.'
                  : 'Onera has no files recorded for this mod.'}
              </p>
            {:else}
              <ul class="tree">
                {#each foldIntoTree(files.entries) as node (node.key)}
                  <li
                    class="node"
                    class:is-file={node.file}
                    class:missing={node.missing}
                    style={`--depth: ${node.depth}`}
                  >
                    <span class="node-mark" aria-hidden="true">{node.file ? '·' : '▸'}</span>
                    <span class="node-name">{node.name}</span>
                    {#if node.missing}
                      <span class="severity-warning">missing</span>
                    {:else if formatSize(node.size) !== null}
                      <span class="node-size">{formatSize(node.size)}</span>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}

            <p class="files-foot">
              <span class="muted">
                {#if files !== undefined}
                  {@const more = remainderNote(files.entries.length, files.total)}
                  {files.total === 1 ? '1 file' : `${files.total} files`}{more !== null
                    ? ` — ${more} not shown`
                    : ''}
                {/if}
              </span>
              <button
                class="browse"
                onclick={() => browse(card)}
                disabled={files === undefined || files.browse === null}
                title={files?.browse ?? 'Onera has nothing on disk for this mod'}
              >
                Browse in files
              </button>
            </p>
          </div>
        {/if}
      </li>
    {/each}
  </CardList>
{/if}
{#if !loading && error === null && mods.length === 0}<p class="empty">
    No mods installed for this game yet. Use <strong>Get mods</strong> to open the listing on Nexus; the
    browser extension sends what you choose back here.
  </p>{/if}

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
  .switcher {
    margin-bottom: 1rem;
  }
  .switcher .selected {
    /* The current installation is not an unavailable control: it is the one
       already chosen, and must not read as greyed out. */
    border-color: var(--accent-line);
    color: var(--accent-strong);
    background: var(--accent-soft);
    opacity: 1;
  }
  /* Each mod is its own raised surface rather than a table row: a card carries
     an image, two dates and a version tag, and a row would make all four
     compete for the same line. */
  .card.update {
    border-color: var(--accent-line);
  }
  /* The expansion is a third row inside the card rather than a panel after it:
     the files belong to this mod, and a separate box would read as a separate
     thing. Wrapping is what lets it take a line of its own. */
  .card {
    flex-wrap: wrap;
    cursor: pointer;
  }
  .card.expanded {
    border-color: var(--line-strong);
  }

  .status {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--ok);
    /* The halo is two shadows: a tight one that reads as light coming off the
       dot, and a wide faint one that gives it depth against the card. */
    box-shadow:
      0 0 0 3px #79d99a1f,
      0 0 10px 2px #79d99a66;
  }
  .status.new {
    background: var(--accent-strong);
    box-shadow:
      0 0 0 3px var(--accent-soft),
      0 0 12px 3px #97a3ff80;
  }

  /* The title is the disclosure control, so it has to look like the heading it
     replaced rather than like a button. */
  .disclosure {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .disclosure:hover {
    color: var(--accent-strong);
  }
  .caret {
    display: inline-block;
    color: var(--faint);
    transition: transform 140ms var(--ease);
  }
  .caret.open {
    transform: rotate(90deg);
  }

  .files {
    flex-basis: 100%;
    min-width: 0;
    margin-top: 0.35rem;
    padding: 0.6rem 0.7rem;
    border: 1px solid var(--line);
    border-radius: var(--radius-sm);
    background: var(--surface-sunken);
    cursor: default;
  }
  .tree {
    list-style: none;
    margin: 0;
    padding: 0;
    font-family: ui-monospace, 'JetBrains Mono', 'SFMono-Regular', Menlo, monospace;
    font-size: 0.8rem;
  }
  .node {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    /* One indent step per level, set inline from the folded tree. */
    padding-left: calc(var(--depth) * 0.9rem);
    line-height: 1.5;
    color: var(--muted);
  }
  .node.is-file {
    color: var(--text);
  }
  .node.missing .node-name {
    text-decoration: line-through;
  }
  .node-mark {
    color: var(--faint);
  }
  .node-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .node-size {
    margin-left: auto;
    color: var(--faint);
  }
  /* The way out of the summary and into the real thing, in the corner where a
     dialog puts its confirming action. */
  .files-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    margin: 0.5rem 0 0;
    font-size: 0.82rem;
  }
  .browse {
    padding: 0.28rem 0.6rem;
    font-size: 0.82rem;
  }
  /* A tile is a fixed square; an expanded one has a list inside it and has to
     be allowed to grow instead. */
  :global(.cards.tiles) .card.expanded {
    aspect-ratio: auto;
  }
  .dates {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem 0.75rem;
    align-items: center;
    margin: 0.25rem 0 0;
    font-size: 0.85rem;
  }
  /* One row of actions rather than a stacked column: four controls in a column
     make every card as tall as its longest button list, and that list is the
     same on every card. */
  .card .actions {
    align-self: center;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .actions .link {
    padding: 0.42rem 0.6rem;
    border-radius: var(--radius-sm);
    color: var(--muted);
    font-size: 0.9rem;
    text-decoration: none;
  }
  .actions .link:hover {
    background: #ffffff0a;
    color: var(--text);
  }
</style>
