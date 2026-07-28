<script lang="ts">
  import { commands } from '$lib/bridge';
  import type { DiscoveredGame, LocalGame } from '$lib/types';
  import { onMount } from 'svelte';

  let discovered = $state<DiscoveredGame[]>([]);
  let registered = $state<LocalGame[]>([]);
  let manualPath = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

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
    await commands.confirmGame(game);
    await refresh();
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

  onMount(refresh);
</script>

<h1>Games</h1>
<p><button onclick={refresh} disabled={busy}>{busy ? 'Scanning…' : 'Scan again'}</button></p>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

<h2>Detected</h2>
{#if discovered.length === 0}
  <p class="muted">
    Nothing detected. Steam libraries are read from Steam's own metadata, so a game installed
    outside Steam needs a manual path below.
  </p>
{/if}
<table>
  <thead><tr><th>Game</th><th>Path</th><th>Source</th><th></th></tr></thead>
  <tbody>
    {#each discovered as game (game.install_root)}
      <tr>
        <td>{game.name}</td>
        <td class="muted">{game.install_root}</td>
        <td class="muted">{game.source.replace('_', ' ')}</td>
        <td>
          {#if isRegistered(game)}
            <span class="muted">added</span>
          {:else if game.validation.valid}
            <!-- Detection is a suggestion: nothing is managed until confirmed,
                 because a wrong match would aim writes at the wrong directory. -->
            <button onclick={() => confirm(game)}>Confirm</button>
          {:else}
            <span class="severity-danger" title={game.validation.findings.join('; ')}
              >not valid</span
            >
          {/if}
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<h2>Add a path manually</h2>
<div class="panel">
  <input
    bind:value={manualPath}
    placeholder="/games/SteamLibrary/steamapps/common/Cyberpunk 2077"
  />
  <p><button onclick={addManual} disabled={manualPath.trim().length === 0}>Add</button></p>
</div>
