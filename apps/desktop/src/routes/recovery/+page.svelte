<script lang="ts">
  import { commands } from '$lib/bridge';
  import type { InterruptedOperation } from '$lib/types';
  import { onMount } from 'svelte';

  let operations = $state<InterruptedOperation[]>([]);
  let error = $state<string | null>(null);

  async function refresh() {
    operations = await commands.interruptedOperations();
  }

  async function rollBack(operationId: string) {
    error = null;
    try {
      await commands.rollBack(operationId);
      await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(refresh);
</script>

<h1>Interrupted operations</h1>
{#if error !== null}<p class="error" role="alert">{error}</p>{/if}

{#if operations.length === 0}
  <p class="muted">Nothing was interrupted. Every operation completed or was rolled back.</p>
{:else}
  <p class="muted">
    Onera found operations that did not finish. Each one's journal records exactly which files were
    already written, so undoing them is safe.
  </p>
  {#each operations as operation (operation.operation_id)}
    <div class="panel">
      <p><strong>{operation.kind}</strong> — {operation.state}</p>
      <p class="muted">
        {operation.committed_files} file(s) written, {operation.staged_files} staged, started {operation.created_at}
      </p>
      {#if operation.recovery !== 'None'}
        <button class="danger" onclick={() => rollBack(operation.operation_id)}>Roll back</button>
      {/if}
    </div>
  {/each}
{/if}
