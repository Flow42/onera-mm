<!--
  The top of the application.

  Everything Onera does is scoped to one installation, so the games are the
  first thing shown and every other section is reached through one of them.
  Detection is kept below the registered games and clearly separated: a
  detected game is a suggestion, and nothing is managed until it is confirmed.
-->
<script lang="ts">
  import CardList from '$lib/components/CardList.svelte';
  import ViewToggle from '$lib/components/ViewToggle.svelte';
  import { commands } from '$lib/bridge';
  import { gameIcon } from '$lib/game-icons.svelte';
  import { gameLabel, initialsOf } from '$lib/navigation';
  import type { DiscoveredGame, LocalGame } from '$lib/types';
  import { viewMode } from '$lib/view-mode.svelte';
  import { onMount } from 'svelte';

  let discovered = $state<DiscoveredGame[]>([]);
  let registered = $state<LocalGame[]>([]);
  let manualPath = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  /** The game just confirmed, so first-time setup can point at the baseline. */
  let justAdded = $state<{ id: string; name: string } | null>(null);

  const layout = viewMode('games', 'tiles');

  async function refresh() {
    busy = true;
    error = null;
    try {
      [discovered, registered] = await Promise.all([
        commands.discoverGames(),
        commands.localGames(),
      ]);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function confirm(game: DiscoveredGame) {
    const id = await commands.confirmGame(game);
    justAdded = { id, name: game.name };
    await refresh();
  }

  /** Click handler: a rejected confirm has to reach the user, not the console. */
  async function confirmClicked(game: DiscoveredGame) {
    error = null;
    try {
      await confirm(game);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function addManual() {
    error = null;
    try {
      const game = await commands.addManualGame(manualPath);
      await confirm(game);
      manualPath = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  const isRegistered = (game: DiscoveredGame) =>
    registered.some((r) => r.install_root === game.install_root);

  const pending = $derived(discovered.filter((game) => !isRegistered(game)));

  /**
   * The name to show for a registered installation.
   *
   * `local_games` carries no title, so a detected game at the same directory is
   * used when there is one and the directory's own name otherwise. Neither is
   * invented: both come from something Onera actually read.
   */
  function nameOf(game: LocalGame): string {
    const match = discovered.find((candidate) => candidate.install_root === game.install_root);
    return match?.name ?? gameLabel(game.install_root);
  }

  onMount(refresh);
</script>

<header class="page-heading">
  <div>
    <h1>Games</h1>
    <p class="lede">
      Every installation Onera manages. Open one to reach its mods, profiles, updates and integrity.
    </p>
  </div>
  <div class="toolbar">
    <ViewToggle mode={layout.current} onchange={(mode) => layout.set(mode)} label="Games layout" />
    <button onclick={refresh} disabled={busy}>{busy ? 'Scanning…' : 'Scan again'}</button>
  </div>
</header>

{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

{#if justAdded !== null}
  <!-- First-time setup: a baseline captured before the first install is what
       makes "return to clean" and byte-for-byte verification possible later.
       After a mod is active it is too late — the capture would record modded
       files as clean, and Onera refuses it. -->
  <div class="panel recommendation" data-testid="baseline-recommendation">
    <h2>Capture a baseline for {justAdded.name}</h2>
    <p>
      Do this <strong>before your first install</strong>, while the game is still untouched. It is
      what lets Onera verify the game byte for byte later and return it to clean.
    </p>
    <p class="muted">
      Once a mod is active, a capture would record modded files as the clean state, so Onera will
      not take one.
    </p>
    <p class="toolbar">
      <a class="cta" href="/integrity?game={justAdded.id}">Capture a baseline</a>
      <button class="ghost" onclick={() => (justAdded = null)}>Later</button>
    </p>
  </div>
{/if}

{#if registered.length === 0}
  <p class="empty">
    No game is registered yet. Confirm a detected one below, or add its directory by hand.
  </p>
{:else}
  <CardList mode={layout.current} label="Registered games">
    {#each registered as game (game.id)}
      <li class="card actionable">
        <!-- The game's own icon when its directory has one; otherwise the
             initials, which is what every game showed before. -->
        <span class="icon" aria-hidden="true">
          {#if gameIcon(game.id) !== null}
            <img src={gameIcon(game.id)} alt="" />
          {:else}
            {initialsOf(nameOf(game))}
          {/if}
        </span>
        <div class="body">
          <h2 class="title">
            <a class="stretch" href={`/games/${encodeURIComponent(game.id)}`}>{nameOf(game)}</a>
          </h2>
          <p class="meta">
            <span class="tag">{game.adapter_id}</span>
            {#if !game.confirmed}<span class="severity-warning">unconfirmed</span>{/if}
          </p>
          <p class="path">{game.install_root}</p>
        </div>
        <span class="chevron above tile-hide" aria-hidden="true">›</span>
      </li>
    {/each}
  </CardList>
{/if}

<h2>Detected</h2>
{#if pending.length === 0}
  <p class="muted">
    Nothing further detected. Steam libraries are read from Steam's own metadata, so a game
    installed outside Steam needs a manual path below.
  </p>
{:else}
  <CardList mode="list" label="Detected games">
    {#each pending as game (game.install_root)}
      <li class="card">
        <span class="icon" aria-hidden="true">{initialsOf(game.name)}</span>
        <div class="body">
          <h3 class="title">{game.name}</h3>
          <p class="meta">
            <span class="tag">{game.source.replace('_', ' ')}</span>
            {#if game.validation.reported_version !== null}
              <span class="muted">version {game.validation.reported_version}</span>
            {/if}
          </p>
          <p class="path">{game.install_root}</p>
        </div>
        <div class="actions">
          {#if game.validation.valid}
            <!-- Detection is a suggestion: nothing is managed until confirmed,
                 because a wrong match would aim writes at the wrong directory. -->
            <button class="primary" onclick={() => confirmClicked(game)}>Confirm</button>
          {:else}
            <span class="severity-danger" title={game.validation.findings.join('; ')}
              >not valid</span
            >
          {/if}
        </div>
      </li>
    {/each}
  </CardList>
{/if}

<h2>Add a path manually</h2>
<div class="panel manual">
  <input
    bind:value={manualPath}
    aria-label="Installation directory"
    placeholder="/games/SteamLibrary/steamapps/common/Cyberpunk 2077"
  />
  <button onclick={addManual} disabled={manualPath.trim().length === 0}>Add</button>
</div>

<style>
  .cta {
    display: inline-block;
    padding: 0.42rem 0.85rem;
    border-radius: var(--radius-sm);
    background: linear-gradient(180deg, var(--accent-strong), var(--accent));
    color: var(--accent-ink);
    font-weight: 600;
    text-decoration: none;
  }
  .recommendation {
    border-left: 3px solid var(--accent);
    margin-bottom: 1.25rem;
  }
  .recommendation h2 {
    margin-top: 0;
  }
  .chevron {
    color: var(--faint);
    font-size: 1.4rem;
    line-height: 1;
  }
  .manual {
    display: flex;
    gap: 0.6rem;
    align-items: center;
  }
</style>
