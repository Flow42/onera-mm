<script lang="ts">
  import { goto } from '$app/navigation';
  import { commands } from '$lib/bridge';
  import type { AccountInfo } from '$lib/types';
  import { onMount } from 'svelte';

  let account = $state<AccountInfo | null>(null);
  let diagnostics = $state<Record<string, string>>({});
  let error = $state<string | null>(null);

  async function signOut() {
    await commands.forgetApiKey();
    await goto('/onboarding');
  }

  onMount(async () => {
    try {
      [account, diagnostics] = await Promise.all([commands.account(), commands.diagnostics()]);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });
</script>

<h1>Settings and diagnostics</h1>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

<h2>Nexus account</h2>
<div class="panel">
  {#if account === null}
    <p class="muted">Not signed in.</p>
  {:else}
    <p>{account.username}{account.premium === true ? ' (premium)' : ''}</p>
    <p class="muted">
      The API key is held in your desktop keyring. Onera never writes it to its database,
      configuration or logs.
    </p>
    <button onclick={() => goto('/onboarding')}>Replace key</button>
    <button class="danger" onclick={signOut}>Delete key</button>
  {/if}
</div>

<h2>Diagnostics</h2>
<table>
  <tbody>
    {#each Object.entries(diagnostics) as [key, value] (key)}
      <tr><td class="muted">{key}</td><td><code>{value}</code></td></tr>
    {/each}
  </tbody>
</table>
