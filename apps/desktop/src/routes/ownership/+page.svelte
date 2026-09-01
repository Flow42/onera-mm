<script lang="ts">
  import { page } from '$app/state';
  import { commands } from '$lib/bridge';
  import type { LocalGame, ProviderStackView } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let gameId = $state('');
  let rootKey = $state('game');
  let path = $state('');
  let stack = $state<ProviderStackView | null>(null);
  let error = $state<string | null>(null);

  async function look() {
    error = null;
    try {
      stack = await commands.ownership(gameId, rootKey, path);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(async () => {
    games = await commands.localGames();
    const requested = page.url.searchParams.get('game');
    gameId = games.find((game) => game.id === requested)?.id ?? games[0]?.id ?? '';
  });
</script>

<h1>File ownership history</h1>
<p class="muted">
  Every deployed path keeps a stack of providers. The bottom entry is what was there before Onera
  touched it; the top is what is on disk now. Removing the top entry restores the one beneath it.
</p>

<div class="panel">
  <label for="game">Game</label>
  <select id="game" bind:value={gameId}>
    {#each games as game (game.id)}<option value={game.id}>{game.adapter_id}</option>{/each}
  </select>
  <label for="root">Deployment root</label>
  <input id="root" bind:value={rootKey} />
  <label for="path">Path</label>
  <input id="path" bind:value={path} placeholder="archive/pc/mod/example.archive" />
  <p><button onclick={look} disabled={path.trim().length === 0}>Look up</button></p>
</div>

{#if error !== null}<p class="error" role="alert">{error}</p>{/if}
{#if stack !== null}
  {#if stack.entries.length === 0}
    <p class="muted">Onera does not manage that path.</p>
  {:else}
    <ol>
      {#each stack.entries as entry, index (entry.hash + String(index))}
        <li>
          {entry.kind === 'unmanaged_backup'
            ? 'File that existed before Onera (backed up)'
            : (entry.mod_name ?? entry.installation_id)}
          <span class="muted">{entry.hash.slice(0, 12)} · {entry.size} bytes</span>
          {#if index === stack.entries.length - 1}<strong> ← deployed</strong>{/if}
        </li>
      {/each}
    </ol>
  {/if}
{/if}
