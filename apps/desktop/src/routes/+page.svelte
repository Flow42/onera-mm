<script lang="ts">
  import { goto } from '$app/navigation';
  import { commands } from '$lib/bridge';
  import { onMount } from 'svelte';

  let message = $state('Starting…');

  onMount(async () => {
    // Onboarding is not optional: without a credential nothing else in the
    // application can do anything useful, so first launch goes straight there.
    try {
      const status = await commands.startupStatus();
      const destination = !status.authenticated
        ? '/onboarding'
        : status.recovery_required
          ? '/recovery'
          : status.inbox_count > 0
            ? '/add'
            : status.expired_plans > 0
              ? `/add?expired=${status.expired_plans}`
              : '/games';
      await goto(destination, { replaceState: true });
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    }
  });
</script>

<p>{message}</p>
