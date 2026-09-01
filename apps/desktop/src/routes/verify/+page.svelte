<script lang="ts">
  import { page } from '$app/state';
  import { commands } from '$lib/bridge';
  import type { LocalGame, VerifyReport } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let report = $state<VerifyReport | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const installation = $derived(page.url.searchParams.get('installation'));
  const requestedGame = $derived(page.url.searchParams.get('game'));

  async function run(gameId: string) {
    if (installation === null) return;
    busy = true;
    error = null;
    try {
      report = await commands.verify(gameId, installation);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  onMount(async () => {
    games = await commands.localGames();
    const game = games.find((candidate) => candidate.id === requestedGame) ?? games[0];
    if (game !== undefined) await run(game.id);
  });
</script>

<h1>Verification</h1>
{#if installation === null}
  <p class="muted">Choose a mod from the installed list to verify it.</p>
{:else if error !== null}
  <p class="error" role="alert">{error}</p>
{:else if busy || report === null}
  <p>Re-reading every file…</p>
{:else}
  <p class="muted">{report.files.length} file(s) checked.</p>
  <table>
    <thead><tr><th>File</th><th>Status</th></tr></thead>
    <tbody>
      {#each report.files as file (file.target)}
        <tr>
          <td><code>{file.target}</code></td>
          <td class={file.status === 'ok' ? 'muted' : 'severity-danger'}>{file.status}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  <p class="muted">
    A modified file is never repaired automatically — repairing would discard edits you may have
    made on purpose.
  </p>
{/if}
