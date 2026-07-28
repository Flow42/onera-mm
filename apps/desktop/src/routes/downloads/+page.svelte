<script lang="ts">
  import { onProgress } from '$lib/bridge';
  import { fraction, initial, reduce, type OperationProgress } from '$lib/progress.svelte';
  import { formatBytes } from '$lib/plan-view';
  import { onMount } from 'svelte';

  // Downloads are performed by the native application, never by the browser, so
  // this view is the only place progress is visible.
  let active = $state<OperationProgress>(initial());

  // `onMount` must return its cleanup synchronously, so the subscription is
  // started in the background and torn down through a captured handle.
  onMount(() => {
    let stop: (() => void) | undefined;
    void onProgress((event) => {
      active = reduce(active, event);
    }).then((unlisten) => {
      stop = unlisten;
    });
    return () => stop?.();
  });
</script>

<h1>Downloads</h1>
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
<p class="muted">
  Interrupted downloads are recorded and resumed on the next launch. Archives are stored by content
  hash, so downloading the same file twice costs nothing.
</p>
