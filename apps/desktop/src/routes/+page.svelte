<script lang="ts">
  import { goto } from '$app/navigation';
  import { commands } from '$lib/bridge';
  import { onMount } from 'svelte';

  let message = $state('Starting…');

  onMount(async () => {
    // Onboarding is not optional: without a credential nothing else in the
    // application can do anything useful, so first launch goes straight there.
    try {
      const authenticated = await commands.isAuthenticated();
      await goto(authenticated ? '/games' : '/onboarding', { replaceState: true });
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    }
  });
</script>

<p>{message}</p>
