<!--
  The shell.

  The application is a menu, not a set of peers: the games list is the top of
  it, a game is one level down, and a game's sections are below that. The rail
  therefore shows one level at a time — the level the current address is on —
  rather than every route at once, and moving between levels is animated in the
  direction of travel so the hierarchy is visible rather than remembered.
-->
<script lang="ts">
  import { page } from '$app/state';
  import { commands, onInbox } from '$lib/bridge';
  import {
    depthOf,
    directionBetween,
    gameLabel,
    gameOf,
    gameSections,
    globalSections,
    initialsOf,
    Level,
  } from '$lib/navigation';
  import { gameIcon } from '$lib/game-icons.svelte';
  import type { InboxOutcome, LocalGame } from '$lib/types';
  import { onMount } from 'svelte';
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import '../app.css';

  const { children } = $props();

  /** The last few things the browser asked for, newest first. */
  let activity = $state<(InboxOutcome & { seq: number })[]>([]);
  let seq = 0;

  /** Registered games, held only so the rail can name the one in context. */
  let games = $state<LocalGame[]>([]);

  onMount(() => {
    // Requests from the browser run whichever page is open, so the notice lives
    // in the shell rather than on the page that happens to be showing.
    const unlisten = onInbox((event) => {
      // Two runs of the same request produce the same identifiers, so the list
      // is keyed by arrival rather than by anything in the payload.
      seq += 1;
      activity = [{ ...event, seq }, ...activity].slice(0, 3);
    });
    // The rail survives a game list it cannot read: it falls back to naming the
    // game by its identifier rather than refusing to draw.
    void commands
      .localGames()
      .then((rows) => (games = rows))
      .catch(() => undefined);
    return () => void unlisten.then((stop) => stop());
  });

  const gameId = $derived(gameOf(page.url.pathname, page.url.search));
  const level = $derived(depthOf(page.url.pathname));
  const sections = $derived(gameId === null ? [] : gameSections(gameId));
  const globals = globalSections();

  /**
   * The games the rail lists.
   *
   * A game the address names but the list does not contain — a stale link, or a
   * list that could not be read — is still shown, so its sections have
   * something to sit under and the rail never loses the game in context.
   */
  const railGames = $derived.by(() => {
    if (gameId === null || games.some((candidate) => candidate.id === gameId)) {
      return games;
    }
    return [...games, { id: gameId, adapter_id: '', install_root: '', confirmed: false }];
  });

  /**
   * Which way the current move travelled.
   *
   * Kept outside the reactive graph deliberately: the previous address is read
   * to decide the direction and must not itself re-trigger the decision.
   */
  let previous = page.url.pathname;
  let direction = $state<'forward' | 'back' | 'none'>('none');
  $effect(() => {
    const next = page.url.pathname;
    if (next === previous) return;
    const moved = directionBetween(previous, next);
    previous = next;
    // A move between two addresses on the same level still animates, but as a
    // small settle rather than as a step deeper into the menu.
    direction = moved;
  });

  /**
   * Whether the movement should be drawn at all.
   *
   * The direction is the point of the animation, not decoration, so a user who
   * has asked for less motion still gets the change of view — just without the
   * travel.
   */
  const reducedMotion =
    typeof globalThis.matchMedia === 'function' &&
    globalThis.matchMedia('(prefers-reduced-motion: reduce)').matches;

  /** Distance, in pixels, a view travels; forward comes from the right. */
  const travel = $derived(
    reducedMotion ? 0 : direction === 'forward' ? 28 : direction === 'back' ? -28 : 0,
  );

  const active = (match: string) => page.url.pathname === match;
</script>

<div class="shell">
  <nav class="rail" aria-label="Sections">
    <a class="brand" href="/games">
      <span class="mark" aria-hidden="true">O</span>
      <span>Onera</span>
    </a>

    <p class="rail-label">Library</p>
    <ul class="rail-list">
      <li>
        <a href="/games" aria-current={level === Level.Games ? 'page' : undefined}>
          <span class="glyph" aria-hidden="true">▦</span>All games
        </a>
      </li>
    </ul>

    <!-- Every registered game is listed, and the one in context grows its own
         submenu underneath it. The sections are indented rather than replacing
         the list, so the address is legible as a place in a hierarchy: this
         game, this section of it. -->
    <ul class="rail-list games">
      {#each railGames as candidate (candidate.id)}
        {@const selected = candidate.id === gameId}
        <li>
          <a
            class="game"
            class:selected
            href={`/games/${encodeURIComponent(candidate.id)}`}
            aria-current={selected && level === Level.Game ? 'page' : undefined}
          >
            <span class="game-icon" aria-hidden="true">
              {#if gameIcon(candidate.id) !== null}
                <img src={gameIcon(candidate.id)} alt="" />
              {:else}
                {initialsOf(
                  candidate.install_root.length > 0 ? gameLabel(candidate.install_root) : '?',
                )}
              {/if}
            </span>
            <span class="game-name">
              {candidate.install_root.length > 0 ? gameLabel(candidate.install_root) : 'This game'}
            </span>
          </a>

          {#if selected}
            <!-- The submenu belongs to the game above it, so it arrives from
                 the right rather than fading in place: the movement is what
                 says "one level deeper", and it is skipped entirely for a user
                 who asked for less motion. -->
            <ul
              class="rail-list submenu"
              transition:fly={{ x: reducedMotion ? 0 : 22, duration: 200, easing: cubicOut }}
            >
              {#each sections as section (section.key)}
                <li>
                  <a href={section.href} aria-current={active(section.match) ? 'page' : undefined}>
                    <span class="glyph" aria-hidden="true">{section.glyph}</span>{section.label}
                  </a>
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>

    <p class="rail-label">Everything else</p>
    <ul class="rail-list">
      {#each globals as section (section.key)}
        <li>
          <a href={section.href} aria-current={active(section.match) ? 'page' : undefined}>
            <span class="glyph" aria-hidden="true">{section.glyph}</span>{section.label}
          </a>
        </li>
      {/each}
    </ul>
  </nav>

  <main>
    {#if activity.length > 0}
      <ul class="activity">
        {#each activity as event (event.seq)}
          <li class:failed={event.outcome === 'failed'}>
            <span>{event.message}</span>
            {#if event.outcome === 'needs_decision'}
              <a href="/add">Review it</a>
            {/if}
          </li>
        {/each}
        <li class="dismiss">
          <button class="ghost" onclick={() => (activity = [])}>Dismiss</button>
        </li>
      </ul>
    {/if}

    <!-- Both views share one grid cell, so the outgoing one does not shift the
         incoming one down the page while the two overlap. -->
    <div class="stage">
      {#key page.url.pathname}
        <div
          class="view"
          in:fly={{ x: travel, duration: 220, delay: 60, easing: cubicOut, opacity: 0 }}
          out:fly={{ x: -travel, duration: 120, easing: cubicOut, opacity: 0 }}
        >
          {@render children()}
        </div>
      {/key}
    </div>
  </main>
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 220px 1fr;
    min-height: 100vh;
  }
  .rail {
    padding: 1rem 0.75rem;
    border-right: 1px solid var(--line);
    background: linear-gradient(180deg, #ffffff05, #ffffff00 40%), #0b0c11cc;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.35rem 0.5rem 1rem;
    font-weight: 620;
    letter-spacing: -0.01em;
    color: var(--text);
    text-decoration: none;
  }
  .mark {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 7px;
    background: linear-gradient(140deg, var(--accent-strong), #5563e0);
    color: var(--accent-ink);
    font-size: 0.8rem;
    font-weight: 700;
  }
  .rail-label {
    margin: 1rem 0 0.35rem;
    padding: 0 0.5rem;
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
  }
  .rail-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .rail a {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.4rem 0.55rem;
    border-radius: 9px;
    color: var(--muted);
    text-decoration: none;
    transition:
      background-color 120ms var(--ease),
      color 120ms var(--ease);
  }
  .rail a:hover {
    background: #ffffff0a;
    color: var(--text);
  }
  .rail a[aria-current='page'] {
    background: var(--accent-soft);
    color: var(--accent-strong);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .glyph {
    width: 1.1em;
    text-align: center;
    opacity: 0.75;
  }
  .games {
    margin-top: 2px;
  }
  /* A game is a taller row than a section: it carries a picture, and it is the
     thing every section below it belongs to. */
  .rail .game {
    gap: 0.5rem;
    padding: 0.35rem 0.5rem;
    font-weight: 550;
    color: var(--text);
  }
  .rail .game.selected {
    color: var(--text);
  }
  .game-icon {
    display: grid;
    place-items: center;
    flex: none;
    width: 22px;
    height: 22px;
    border-radius: 6px;
    border: 1px solid var(--line);
    background: var(--surface);
    overflow: hidden;
    font-size: 0.62rem;
    font-weight: 700;
    color: var(--faint);
  }
  .game-icon img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .game-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The indent is the whole point: these entries are inside the game above
     them, and the line down the left says where they stop. */
  .submenu {
    margin: 2px 0 0.5rem;
    padding-left: 0.75rem;
    margin-left: 0.6rem;
    border-left: 1px solid var(--line);
  }
  .submenu a {
    font-size: 0.92rem;
    padding: 0.32rem 0.5rem;
  }
  main {
    padding: 1.75rem 2rem 3rem;
    overflow: auto;
    max-width: 1180px;
    width: 100%;
  }
  .stage {
    display: grid;
  }
  .view {
    grid-area: 1 / 1;
    min-width: 0;
  }
  .activity {
    list-style: none;
    margin: 0 0 1.25rem;
    padding: 0.6rem 0.85rem;
    border: 1px solid var(--line);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow-card);
    display: grid;
    gap: 0.35rem;
  }
  .activity li {
    display: flex;
    gap: 0.75rem;
    align-items: center;
  }
  .activity li.failed {
    color: var(--danger);
  }
  .activity .dismiss {
    justify-content: flex-end;
  }
</style>
