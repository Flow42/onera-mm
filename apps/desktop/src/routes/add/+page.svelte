<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { commands } from '$lib/bridge';
  import { formatBytes } from '$lib/plan-view';
  import type { InboxRequest, LocalGame, ModDetails } from '$lib/types';
  import { onMount } from 'svelte';
  import { SvelteURLSearchParams } from 'svelte/reactivity';

  let games = $state<LocalGame[]>([]);
  let inbox = $state<InboxRequest[]>([]);
  let gameId = $state('');
  let gameDomain = $state('');
  let providerModId = $state('');
  let details = $state<ModDetails | null>(null);
  let selectedFile = $state('');
  let activeRequest = $state<InboxRequest | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function openModPage(adapterId: string) {
    error = null;
    try {
      await commands.openModPage(adapterId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshInbox() {
    inbox = await commands.inboxRequests();
  }

  async function fetchDetails() {
    busy = true;
    error = null;
    try {
      details = await commands.fetchMod(gameDomain.trim(), providerModId.trim());
      selectedFile =
        activeRequest?.provider_file_id ??
        details.files.find((file) => file.is_primary)?.id ??
        details.files[0]?.id ??
        '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      details = null;
    } finally {
      busy = false;
    }
  }

  async function openRequest(request: InboxRequest) {
    activeRequest = request;
    gameDomain = request.game_slug;
    providerModId = request.provider_mod_id;
    await fetchDetails();
  }

  /**
   * Take a request out of the inbox, stopping it if it has already started.
   *
   * One action for both cases deliberately: from here a request that is queued,
   * one the desktop is downloading right now and one that failed all look the
   * same, and the user's intent — stop doing this — does not change with the
   * state it happens to be in. Only the label does.
   */
  async function cancel(request: InboxRequest) {
    error = null;
    try {
      await commands.cancelInboxRequest(request.id);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    if (activeRequest?.id === request.id) activeRequest = null;
    await refreshInbox();
  }

  async function downloaded() {
    if (details === null || selectedFile === '') return;
    busy = true;
    error = null;
    try {
      await commands.downloadFile(gameDomain, providerModId, selectedFile);
      if (activeRequest !== null) await commands.completeInboxRequest(activeRequest.id);
      await goto('/downloads');
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      busy = false;
    }
  }

  async function install() {
    if (details === null || selectedFile === '' || gameId === '') return;
    const query = new SvelteURLSearchParams({
      game: gameId,
      domain: gameDomain,
      mod: providerModId,
      file: selectedFile,
    });
    if (activeRequest !== null) query.set('request', activeRequest.id);
    await goto(`/install?${query.toString()}`);
  }

  onMount(async () => {
    [games, inbox] = await Promise.all([commands.localGames(), commands.inboxRequests()]);
    gameId = games[0]?.id ?? '';
    if (inbox[0] !== undefined) await openRequest(inbox[0]);
  });
</script>

<h1>Add a mod</h1>
{#if Number(page.url.searchParams.get('expired') ?? 0) > 0}
  <p class="severity-warning">
    A previous installation preview expired when Onera restarted. Its staged files were cleaned up;
    select the mod again to create a fresh preview.
  </p>
{/if}
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

{#if inbox.length > 0}
  <h2>From your browser</h2>
  {#each inbox as request (request.id)}
    <div class="panel request">
      <span
        >{request.kind.replaceAll('_', ' ')}: {request.game_slug}/mods/{request.provider_mod_id}</span
      >
      <button onclick={() => openRequest(request)}>Open</button>
      <button onclick={() => cancel(request)}>
        {request.state === 'failed' ? 'Dismiss' : 'Cancel'}
      </button>
      {#if request.error !== null}<span class="severity-danger">{request.error}</span>{/if}
    </div>
  {/each}
{/if}

{#if inbox.length === 0 && details === null}
  <div class="panel">
    <p>
      Mods are added from the browser. On a mod's Nexus page, use the Onera browser extension's
      download button and the request appears here, already tied to the file you picked.
    </p>
    <p class="muted">
      Onera does not browse the catalogue itself: the page you were looking at is the only place
      that knows which file of a mod you meant.
    </p>
    {#each games as game (game.id)}
      <p>
        <button onclick={() => openModPage(game.adapter_id)}>
          Get mods for {game.adapter_id}
        </button>
        <span class="muted">{game.install_root}</span>
      </p>
    {/each}
    {#if games.length === 0}
      <p class="severity-warning">Register a game first — a mod is always installed into one.</p>
    {/if}
  </div>
{/if}

{#if details !== null}
  <h2>{details.name}</h2>
  <div class="panel">
    {#if details.author !== null}<p class="muted">by {details.author}</p>{/if}
    <label for="file">File</label>
    <select id="file" bind:value={selectedFile}>
      {#each details.files as file (file.id)}
        <option value={file.id}>
          {file.name}{file.size === null ? '' : ` (${formatBytes(file.size)})`}{file.is_primary
            ? ' — primary'
            : ''}
        </option>
      {/each}
    </select>
    <label for="game">Install into</label>
    <select id="game" bind:value={gameId}>
      {#each games as game (game.id)}<option value={game.id}
          >{game.adapter_id} — {game.install_root}</option
        >{/each}
    </select>
    {#if games.length === 0}<p class="severity-warning">
        Register a compatible game before installing.
      </p>{/if}
    <p>
      <button onclick={downloaded} disabled={busy || selectedFile === ''}>Download only</button>
      <button
        class="primary"
        onclick={install}
        disabled={busy || selectedFile === '' || gameId === ''}
      >
        Preview installation
      </button>
    </p>
  </div>
{/if}

<style>
  .request {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 0.5rem;
  }
  .request span:first-child {
    flex: 1;
  }
  select {
    width: 100%;
    margin-bottom: 0.75rem;
  }
</style>
