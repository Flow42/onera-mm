<script lang="ts">
  import { onProgress } from '$lib/bridge';
  import { fraction, initial, reduce, type OperationProgress } from '$lib/progress.svelte';
  import { formatBytes } from '$lib/plan-view';
  import { commands } from '$lib/bridge';
  import type { DownloadJob } from '$lib/types';
  import { onMount } from 'svelte';

  // Downloads are performed by the native application, never by the browser, so
  // this view is the only place progress is visible.
  let active = $state<OperationProgress>(initial());
  let jobs = $state<DownloadJob[]>([]);
  let busy = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);

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

<h1>Downloads</h1>
<p><button onclick={resume} disabled={busy}>{busy ? 'Resuming…' : 'Resume incomplete'}</button></p>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}
<div class="panel">
  <p>{active.stage}{active.detail === null ? '' : `: ${active.detail}`}</p>
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
{:else}<table>
    <thead><tr><th>File</th><th>Status</th><th>Progress</th><th>Attempts</th></tr></thead>
    <tbody>
      {#each jobs as job (job.id)}
        <tr>
          <td
            >{job.filename}<br /><span class="muted">{job.game_slug} / {job.provider_mod_id}</span
            ></td
          >
          <td class:severity-danger={job.state === 'failed'}>{job.state}</td>
          <td>
            {formatBytes(job.bytes_downloaded)}{job.expected_size === null
              ? ''
              : ` / ${formatBytes(job.expected_size)}`}
          </td>
          <td>{job.attempts}</td>
        </tr>
        {#if job.error !== null}<tr><td colspan="4" class="severity-danger">{job.error}</td></tr
          >{/if}
      {/each}
    </tbody>
  </table>{/if}
{#if !loading && error === null && jobs.length === 0}<p class="muted">No downloads yet.</p>{/if}
