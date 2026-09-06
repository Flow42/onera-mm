<!--
  The transfer queue.

  Deliberately global rather than per game: the queue is shared, and a job
  names the provider's own game slug, which is not the same identifier as a
  registered installation. Filtering it by the game the user came from would be
  a guess, so every job is shown and each says which slug it belongs to.
-->
<script lang="ts">
  import CardList from '$lib/components/CardList.svelte';
  import ViewToggle from '$lib/components/ViewToggle.svelte';
  import { onProgress } from '$lib/bridge';
  import { fraction, initial, reduce, type OperationProgress } from '$lib/progress.svelte';
  import { formatBytes } from '$lib/plan-view';
  import { commands } from '$lib/bridge';
  import type { DownloadJob } from '$lib/types';
  import { viewMode } from '$lib/view-mode.svelte';
  import { onMount } from 'svelte';

  // Downloads are performed by the native application, never by the browser, so
  // this view is the only place progress is visible.
  let active = $state<OperationProgress>(initial());
  let jobs = $state<DownloadJob[]>([]);
  let busy = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);

  const layout = viewMode('downloads', 'list');

  async function refresh() {
    loading = true;
    error = null;
    try {
      jobs = await commands.downloads();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function resume() {
    busy = true;
    error = null;
    try {
      await commands.resumeDownloads();
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /**
   * Stop one transfer.
   *
   * The queue is refreshed rather than the row edited: what a cancel leaves
   * behind — how many bytes it kept, whether it had already finished — is the
   * backend's answer, not something this view should guess at.
   */
  async function cancel(job: DownloadJob) {
    error = null;
    try {
      await commands.cancelDownload(job.id);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
    await refresh();
  }

  /** Whether a job still has work that can be stopped. */
  function cancellable(job: DownloadJob): boolean {
    return job.state === 'queued' || job.state === 'running' || job.state === 'paused';
  }

  /**
   * How far one job has got, as a fraction.
   *
   * Null when the provider never reported a size: a job of unknown length is
   * shown as indeterminate rather than as a bar that would have to invent a
   * denominator.
   */
  function progressOf(job: DownloadJob): number | null {
    return job.expected_size === null || job.expected_size <= 0
      ? null
      : Math.min(1, job.bytes_downloaded / job.expected_size);
  }

  // `onMount` must return its cleanup synchronously, so the subscription is
  // started in the background and torn down through a captured handle.
  onMount(() => {
    let stop: (() => void) | undefined;
    void onProgress((event) => {
      active = reduce(active, event);
    }).then((unlisten) => {
      stop = unlisten;
    });
    void refresh();
    return () => stop?.();
  });
</script>

<header class="page-heading">
  <div>
    <h1>Downloads</h1>
    <p class="lede">
      The queue is shared across every game. Transfers are performed by Onera itself, never by the
      browser.
    </p>
  </div>
  <div class="toolbar">
    <ViewToggle
      mode={layout.current}
      onchange={(mode) => layout.set(mode)}
      label="Downloads layout"
    />
    <button onclick={resume} disabled={busy}>{busy ? 'Resuming…' : 'Resume incomplete'}</button>
  </div>
</header>

{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

<div class="panel current">
  <p class="stage">{active.stage}{active.detail === null ? '' : `: ${active.detail}`}</p>
  {#if fraction(active) === null}
    <progress></progress>
  {:else}
    <progress value={fraction(active)}></progress>
    <p class="muted">{formatBytes(active.completed)} of {formatBytes(active.total ?? 0)}</p>
  {/if}
  {#each active.warnings as warning (warning)}<p class="severity-warning">{warning}</p>{/each}
</div>

{#if loading}
  <p class="muted">Loading downloads…</p>
{:else}
  <CardList mode={layout.current} label="Download jobs">
    {#each jobs as job (job.id)}
      <li class="card" class:failed={job.state === 'failed'}>
        <span class="icon" aria-hidden="true">⇣</span>
        <div class="body">
          <h2 class="title">{job.filename}</h2>
          <p class="meta">
            <span class="tag" class:severity-danger={job.state === 'failed'}>{job.state}</span>
            <span class="muted"
              >{formatBytes(job.bytes_downloaded)}{job.expected_size === null
                ? ''
                : ` / ${formatBytes(job.expected_size)}`}</span
            >
            <span class="muted tile-hide">attempt {job.attempts}</span>
          </p>
          {#if progressOf(job) === null}
            <p class="muted tile-hide">The provider did not report a size for this file.</p>
          {:else}
            <progress value={progressOf(job)}></progress>
          {/if}
          <p class="path">{job.game_slug} / {job.provider_mod_id}</p>
          {#if job.error !== null}<p class="severity-danger">{job.error}</p>{/if}
        </div>
        {#if cancellable(job)}
          <button class="cancel" onclick={() => cancel(job)}>Cancel</button>
        {/if}
      </li>
    {/each}
  </CardList>
{/if}
{#if !loading && error === null && jobs.length === 0}<p class="empty">No downloads yet.</p>{/if}

<style>
  .current {
    margin-bottom: 1.25rem;
  }
  .stage {
    margin: 0 0 0.5rem;
    font-weight: 500;
  }
  /* A job carries several lines — a bar, its slug, sometimes an error — so the
     mark sits at the top of them rather than floating in the middle. */
  .card {
    align-items: flex-start;
  }
  .card.failed {
    border-color: #ff6f8659;
  }
  .card progress {
    margin-top: 0.45rem;
  }
  /* Beside the job rather than under it: it acts on the whole row, and a
     button in the flow of the text would read as part of the description. */
  .cancel {
    align-self: center;
    margin-left: auto;
  }
</style>
