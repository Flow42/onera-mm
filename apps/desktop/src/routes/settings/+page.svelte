<script lang="ts">
  import { goto } from '$app/navigation';
  import { commands } from '$lib/bridge';
  import type { AccountInfo, DownloadPaths } from '$lib/types';
  import { onMount } from 'svelte';

  let account = $state<AccountInfo | null>(null);
  let diagnostics = $state<Record<string, string>>({});
  let error = $state<string | null>(null);

  /** The directory every game downloads into unless it has one of its own. */
  let downloads = $state<DownloadPaths | null>(null);
  let downloadNotice = $state<string | null>(null);
  let downloadError = $state<string | null>(null);
  let changingDownloads = $state(false);

  async function signOut() {
    await commands.forgetApiKey();
    await goto('/onboarding');
  }

  async function loadDownloads() {
    try {
      downloads = await commands.downloadPaths();
    } catch {
      // A settings page that cannot read one setting still shows the others.
      downloads = null;
    }
  }

  /** How a move reads once it has happened. */
  function movedText(change: { moved: number; root: string; previous: string }): string {
    if (change.moved === 0) {
      return `Downloads now go to ${change.root}.`;
    }
    const what = change.moved === 1 ? '1 archive was' : `${change.moved} archives were`;
    return `Downloads now go to ${change.root}. ${what} moved there from ${change.previous}.`;
  }

  /**
   * Change where downloads are kept.
   *
   * The picker, the checks and the move all happen in one backend call: a
   * setting that pointed at a directory the archives never reached would leave
   * the catalogue naming files that are not there.
   */
  async function chooseDownloads() {
    changingDownloads = true;
    downloadError = null;
    downloadNotice = null;
    try {
      const change = await commands.pickDownloadRoot();
      if (change === null) return; // Cancelled: nothing to say.
      downloadNotice = movedText(change);
      await loadDownloads();
    } catch (e) {
      downloadError = e instanceof Error ? e.message : String(e);
    } finally {
      changingDownloads = false;
    }
  }

  /** Put downloads back in Onera's own directory. */
  async function resetDownloads() {
    changingDownloads = true;
    downloadError = null;
    downloadNotice = null;
    try {
      downloadNotice = movedText(await commands.resetDownloadRoot());
      await loadDownloads();
    } catch (e) {
      downloadError = e instanceof Error ? e.message : String(e);
    } finally {
      changingDownloads = false;
    }
  }

  onMount(async () => {
    void loadDownloads();
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

<h2>Downloads</h2>
<div class="panel">
  {#if downloads === null}
    <p class="muted">Reading the download directory…</p>
  {:else}
    <p class="location-label">
      Where downloaded mods are kept
      {#if downloads.scope === 'default'}<span class="tag">Onera's own</span>{/if}
      {#if downloads.archives > 0}<span class="tag">{downloads.archives} archives</span>{/if}
    </p>
    <p class="path">{downloads.root}</p>
    <p class="toolbar">
      <button onclick={chooseDownloads} disabled={changingDownloads}>
        {changingDownloads ? 'Working…' : 'Change…'}
      </button>
      {#if downloads.scope !== 'default'}
        <button class="ghost" onclick={resetDownloads} disabled={changingDownloads}>
          Use Onera's own
        </button>
      {/if}
    </p>
    <p class="muted note">
      Every game downloads here unless it has been given a directory of its own, which is set on
      that game's page. Archives are kept by content, so a mod that two games share is stored once.
      Changing this moves what is already here — the database follows the files, so nothing is
      downloaded twice.
    </p>
    {#if downloadNotice !== null}<p class="notice">{downloadNotice}</p>{/if}
    {#if downloadError !== null}<p class="error" role="alert">{downloadError}</p>{/if}
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

<style>
  .location-label {
    margin: 0 0 0.2rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--faint);
  }
  .path {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .note {
    margin: 0.4rem 0 0;
    max-width: 68ch;
    font-size: 0.85rem;
  }
  .notice {
    margin: 0.5rem 0 0;
    color: var(--accent-strong);
    font-size: 0.9rem;
  }
</style>
