<script lang="ts">
  import { commands } from '$lib/bridge';
  import type { InstalledMod, LocalGame } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let updates = $state<InstalledMod[]>([]);
  let checking = $state(false);
  let error = $state<string | null>(null);

  async function check() {
    checking = true;
    error = null;
    try {
      const results = await Promise.all(games.map((g) => commands.checkUpdates(g.id)));
      updates = results.flat().filter((m) => m.update_available);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      checking = false;
    }
  }

  onMount(async () => {
    try {
      games = await commands.localGames();
      await check();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });
</script>

<h1>Updates</h1>
<p><button onclick={check} disabled={checking}>{checking ? 'Checking…' : 'Check again'}</button></p>
{#if error !== null}<p class="error" role="alert">
    {error} You can retry when the provider is reachable.
  </p>{/if}

{#if checking}
  <p class="muted">Checking installed mods…</p>
{:else if error === null && updates.length === 0}
  <p class="muted">Everything is up to date.</p>
{:else}
  <table>
    <thead><tr><th>Mod</th><th>Installed</th><th>Available</th></tr></thead>
    <tbody>
      {#each updates as mod (mod.installation_id)}
        <tr>
          <!-- Both versions are shown exactly as their author published them.
               Onera compares publication dates, never version strings. -->
          <td>{mod.name}</td>
          <td class="muted">{mod.version}</td>
          <td>{mod.latest_version ?? 'newer release'}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}
