<script lang="ts">
  import { commands } from '$lib/bridge';
  import type { InstalledMod, LocalGame } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let updates = $state<InstalledMod[]>([]);
  let checking = $state(false);

  async function check() {
    checking = true;
    try {
      const results = await Promise.all(games.map((g) => commands.checkUpdates(g.id)));
      updates = results.flat().filter((m) => m.update_available);
    } finally {
      checking = false;
    }
  }

  onMount(async () => {
    games = await commands.localGames();
    await check();
  });
</script>

<h1>Updates</h1>
<p><button onclick={check} disabled={checking}>{checking ? 'Checking…' : 'Check again'}</button></p>

{#if updates.length === 0}
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
