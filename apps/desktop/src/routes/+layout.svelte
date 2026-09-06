<script lang="ts">
  import { page } from '$app/state';
  import { onInbox } from '$lib/bridge';
  import type { InboxOutcome } from '$lib/types';
  import { onMount } from 'svelte';
  import '../app.css';

  const { children } = $props();

  /** The last few things the browser asked for, newest first. */
  let activity = $state<(InboxOutcome & { seq: number })[]>([]);
  let seq = 0;

  onMount(() => {
    // Requests from the browser run whichever page is open, so the notice lives
    // in the shell rather than on the page that happens to be showing.
    const unlisten = onInbox((event) => {
      // Two runs of the same request produce the same identifiers, so the list
      // is keyed by arrival rather than by anything in the payload.
      seq += 1;
      activity = [{ ...event, seq }, ...activity].slice(0, 3);
    });
    return () => void unlisten.then((stop) => stop());
  });

  const sections = [
    { href: '/games', label: 'Games' },
    { href: '/profiles', label: 'Profiles' },
    { href: '/mods', label: 'Installed' },
    { href: '/updates', label: 'Updates' },
    { href: '/downloads', label: 'Downloads' },
    { href: '/verify', label: 'Verify' },
    { href: '/integrity', label: 'Integrity' },
    { href: '/recovery', label: 'Recovery' },
    { href: '/settings', label: 'Settings' },
  ];
</script>

<div class="shell">
  <nav aria-label="Sections">
    <strong class="brand">Onera</strong>
    <ul>
      {#each sections as section (section.href)}
        <li>
          <a
            href={section.href}
            aria-current={page.url.pathname.startsWith(section.href) ? 'page' : undefined}
          >
            {section.label}
          </a>
        </li>
      {/each}
    </ul>
  </nav>
  <main>
    {#if activity.length > 0}
      <ul class="activity">
        {#each activity as event (event.seq)}
          <li class:failed={event.outcome === 'failed'}>
            <span>{event.message}</span>
            {#if event.outcome === 'needs_decision'}
              <a href="/add">Review it</a>
            {/if}
          </li>
        {/each}
        <li class="dismiss">
          <button onclick={() => (activity = [])}>Dismiss</button>
        </li>
      </ul>
    {/if}
    {@render children()}
  </main>
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 180px 1fr;
    min-height: 100vh;
  }
  nav {
    padding: 1rem;
    border-right: 1px solid var(--line);
  }
  .brand {
    display: block;
    margin-bottom: 1rem;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.25rem;
  }
  a {
    display: block;
    padding: 0.4rem 0.5rem;
    border-radius: 6px;
    color: inherit;
    text-decoration: none;
  }
  a[aria-current='page'] {
    background: var(--accent-soft);
  }
  main {
    padding: 1.5rem;
    overflow: auto;
  }
  .activity {
    list-style: none;
    margin: 0 0 1rem;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--line);
    border-left: 3px solid var(--accent);
    border-radius: 8px;
    background: var(--panel);
    display: grid;
    gap: 0.35rem;
  }
  .activity li {
    display: flex;
    gap: 0.75rem;
    align-items: center;
  }
  .activity li.failed {
    color: var(--danger);
  }
  .activity .dismiss {
    justify-content: flex-end;
  }
</style>
