<script lang="ts">
  import { commands } from '$lib/bridge';
  import type { InstalledMod, LocalGame } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let selected = $state<string | null>(null);
  let mods = $state<InstalledMod[]>([]);
  let error = $state<string | null>(null);

  async function load(gameId: string) {
    selected = gameId;
    error = null;
    try {
      mods = await commands.installedMods(gameId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function remove(mod: InstalledMod) {
    if (selected === null) return;
    // The preview is shown first because removal restores previous providers and
    // unmanaged originals; the user should see that before it happens.
    const preview = await commands.previewRemoval(selected, mod.installation_id);
    const summary =
      `${preview.deleted.length} deleted, ${preview.restored.length} restored, ` +
      `${preview.kept_shared.length} kept (shared by another mod)`;
    if (!window.confirm(`Remove ${mod.name}?\n\n${summary}`)) return;
    await commands.remove(selected, mod.installation_id, false);
    await load(selected);
  }

  onMount(async () => {
    games = await commands.localGames();
    if (games[0] !== undefined) await load(games[0].id);
  });
</script>

<h1>Installed mods</h1>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

<p>
  {#each games as game (game.id)}
    <button onclick={() => load(game.id)} disabled={selected === game.id}>{game.adapter_id}</button>
  {/each}
</p>

<table>
  <thead><tr><th>Mod</th><th>Version</th><th>Installed</th><th></th></tr></thead>
  <tbody>
    {#each mods as mod (mod.installation_id)}
      <tr>
        <!-- The version is whatever the author published, shown verbatim. -->
        <td>{mod.name}</td>
        <td class="muted">{mod.version}</td>
        <td class="muted">{mod.installed_at}</td>
        <td>
          <a href={`/verify?installation=${mod.installation_id}`}>Verify</a>
          <button class="danger" onclick={() => remove(mod)}>Remove</button>
        </td>
      </tr>
    {/each}
  </tbody>
</table>
{#if mods.length === 0}<p class="muted">No mods installed for this game yet.</p>{/if}
