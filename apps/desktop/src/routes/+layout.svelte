<script lang="ts">
  import { page } from '$app/state';
  import '../app.css';

  const { children } = $props();

  const sections = [
    { href: '/games', label: 'Games' },
    { href: '/add', label: 'Add mods' },
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
  <main>{@render children()}</main>
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
</style>
