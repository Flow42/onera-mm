<script lang="ts">
  import { goto } from '$app/navigation';
  import { commands } from '$lib/bridge';
  import type { AccountInfo } from '$lib/types';

  let key = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let account = $state<AccountInfo | null>(null);

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    try {
      // The key is validated against Nexus and stored in the Secret Service by
      // the backend. It is never held here beyond this handler.
      account = await commands.setApiKey(key);
      key = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<h1>Connect to Nexus Mods</h1>

{#if account === null}
  <form onsubmit={submit} class="panel">
    <p class="muted">
      Onera needs a personal API key from
      <code>nexusmods.com/users/myaccount?tab=api</code>. It is stored in your desktop keyring and
      never written to Onera's database, configuration or logs.
    </p>
    <label for="api-key">Personal API key</label>
    <!-- Masked: the key must not be shoulder-surfable or screenshot-visible. -->
    <input id="api-key" type="password" bind:value={key} autocomplete="off" spellcheck="false" />
    {#if error !== null}<p class="error" role="alert">{error}</p>{/if}
    <p>
      <button class="primary" type="submit" disabled={busy || key.trim().length === 0}>
        {busy ? 'Checking…' : 'Validate and save'}
      </button>
    </p>
  </form>
{:else}
  <div class="panel">
    <p>
      Signed in as <strong>{account.username}</strong>{account.premium === true
        ? ' (premium)'
        : ''}.
    </p>
    <button class="primary" onclick={() => goto('/games')}>Find my games</button>
  </div>
{/if}
