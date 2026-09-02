<script lang="ts">
  import { page } from '$app/state';
  import { commands } from '$lib/bridge';
  import {
    bytes,
    buildLabel,
    classificationCopy,
    differences,
    freshnessCopy,
    sourceCopy,
    verdict,
  } from '$lib/baseline-view';
  import type {
    BaselineCapturePreview,
    BaselineStatus,
    BaselineVerification,
    CleanRestorePreview,
    CleanRestoreReport,
    LocalGame,
  } from '$lib/types';
  import { onMount } from 'svelte';

  let games = $state<LocalGame[]>([]);
  let selected = $state<string | null>(null);
  let status = $state<BaselineStatus | null>(null);
  let capturePreview = $state<BaselineCapturePreview | null>(null);
  let verification = $state<BaselineVerification | null>(null);
  let cleanPreview = $state<CleanRestorePreview | null>(null);
  let cleanReport = $state<CleanRestoreReport | null>(null);
  let storeVerified = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<string | null>(null);

  const freshness = $derived(status === null ? null : freshnessCopy(status.freshness));
  const source = $derived(
    status === null || status.baseline === null ? null : sourceCopy(status.baseline.source),
  );
  const outcome = $derived(
    verification === null ? null : verdict(verification, status?.baseline ?? null),
  );
  /**
   * Whether the user must confirm the store's own file verification first.
   *
   * Assumed until a preview says otherwise: guessing "no confirmation needed"
   * before Onera has said so is the one direction that could record a weaker
   * claim as a stronger one.
   */
  const needsConfirmation = $derived(
    capturePreview === null || capturePreview.requires_store_verification,
  );
  /** A store-verified capture cannot start until the user confirms the store check. */
  const captureReady = $derived(
    status !== null &&
      status.capture_blocked_reason === null &&
      (!needsConfirmation || storeVerified),
  );

  async function run<T>(label: string, work: () => Promise<T>): Promise<T | null> {
    busy = label;
    error = null;
    try {
      return await work();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      return null;
    } finally {
      busy = null;
    }
  }

  async function select(gameId: string) {
    selected = gameId;
    verification = null;
    capturePreview = null;
    cleanPreview = null;
    cleanReport = null;
    storeVerified = false;
    status = await run('Reading', () => commands.baselineStatus(gameId));
  }

  async function preview() {
    if (selected === null) return;
    capturePreview = await run('Measuring', () => commands.planBaselineCapture(selected!));
  }

  async function capture() {
    if (selected === null) return;
    const result = await run('Hashing', () => commands.captureBaseline(selected!, storeVerified));
    if (result !== null) {
      capturePreview = null;
      await select(selected);
    }
  }

  async function check(quick: boolean) {
    if (selected === null) return;
    cleanPreview = null;
    cleanReport = null;
    verification = await run(quick ? 'Checking sizes' : 'Re-hashing', () =>
      commands.verifyBaseline(selected!, quick),
    );
  }

  async function previewClean() {
    if (selected === null) return;
    cleanReport = null;
    cleanPreview = await run('Planning', () => commands.planReturnToClean(selected!));
  }

  async function applyClean() {
    if (selected === null) return;
    const report = await run('Restoring', () => commands.applyReturnToClean(selected!));
    if (report !== null) {
      cleanReport = report;
      cleanPreview = null;
      verification = report.verification;
      await select(selected);
      cleanReport = report;
    }
  }

  onMount(async () => {
    games = await commands.localGames();
    const requested = page.url.searchParams.get('game');
    const game = games.find((candidate) => candidate.id === requested) ?? games[0];
    if (game !== undefined) await select(game.id);
  });
</script>

<h1>Game integrity</h1>

{#if games.length === 0}
  <p class="muted">Register a game first — integrity is scoped to one installation.</p>
{:else}
  <p>
    <label>
      Installation
      <select
        value={selected}
        onchange={(event) => select((event.currentTarget as HTMLSelectElement).value)}
      >
        {#each games as game (game.id)}
          <option value={game.id}>{game.install_root}</option>
        {/each}
      </select>
    </label>
  </p>
{/if}

{#if error !== null}<p class="error" role="alert">{error}</p>{/if}
{#if busy !== null}<p aria-live="polite">{busy}…</p>{/if}

{#if status !== null}
  <!-- ------------------------------------------------------------------ -->
  <!-- The baseline itself                                                -->
  <!-- ------------------------------------------------------------------ -->
  <section class="panel">
    <h2>Baseline</h2>
    {#if status.baseline === null}
      <p>
        No baseline has been captured. Onera does not yet know what this installation looks like
        when it is clean.
      </p>
      <p class="muted">
        Capturing one before your first install is what makes “return to clean” and byte-for-byte
        verification possible later.
      </p>
    {:else}
      <dl>
        <dt>Source</dt>
        <dd>
          <span class="severity-{source?.severity}">{source?.label}</span>
          <span class="muted">{source?.detail}</span>
        </dd>
        <dt>Build identity</dt>
        <dd>{buildLabel(status.baseline.build_identity)}</dd>
        <dt>Captured</dt>
        <dd>{status.baseline.captured_at}</dd>
        <dt>Contents</dt>
        <dd>
          {status.baseline.file_count} file(s), {bytes(status.baseline.total_bytes)}
          {#if status.baseline.reported_version !== null}
            <span class="muted">· game reports {status.baseline.reported_version}</span>
          {/if}
        </dd>
      </dl>
    {/if}

    <p>
      <strong>Freshness:</strong>
      <span class="severity-{freshness?.severity}" data-testid="freshness">{freshness?.label}</span>
      <span class="muted">{freshness?.detail}</span>
    </p>
    {#if status.observed_build_identity !== null}
      <p class="muted">Installed now: {buildLabel(status.observed_build_identity)}</p>
    {/if}
  </section>

  <!-- ------------------------------------------------------------------ -->
  <!-- Capture                                                            -->
  <!-- ------------------------------------------------------------------ -->
  <section class="panel">
    <h2>{status.baseline === null ? 'Capture a baseline' : 'Replace the baseline'}</h2>
    {#if status.capture_blocked_reason !== null}
      <p class="severity-warning" role="status">{status.capture_blocked_reason}</p>
      <p class="muted">
        A baseline captured over Onera's own deployments would record modded files as clean.
      </p>
    {:else}
      <p>
        <button onclick={preview} disabled={busy !== null}>What will be scanned?</button>
      </p>
      {#if capturePreview !== null}
        <p class="muted">
          {capturePreview.roots.length} root(s), about {capturePreview.estimated_files} file(s) and
          {bytes(capturePreview.estimated_bytes)} to hash.
        </p>
        <table>
          <thead><tr><th>Excluded</th><th>Why</th></tr></thead>
          <tbody>
            {#each capturePreview.exclusions as exclusion, i (i)}
              <tr>
                <td
                  ><code
                    >{exclusion.pattern.kind}: {exclusion.pattern.path ??
                      exclusion.pattern.extension ??
                      exclusion.pattern.name}</code
                  ></td
                >
                <td class="muted">{exclusion.note ?? exclusion.reason}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
      {#if needsConfirmation}
        <p>
          <label>
            <input type="checkbox" bind:checked={storeVerified} />
            I ran the store's own “verify installed files” and it finished.
          </label>
        </p>
        <p class="muted">
          Onera cannot check this for you. The capture is a local observation stamped with the build
          it saw — not a claim that the store attested every byte.
        </p>
      {:else}
        <p class="muted">
          This installation was not added from a store, so its baseline is a clearly labelled local
          snapshot.
        </p>
      {/if}
      <p>
        <button class="primary" onclick={capture} disabled={busy !== null || !captureReady}>
          {status.baseline === null ? 'Capture' : 'Replace stale baseline'}
        </button>
      </p>
    {/if}
  </section>

  <!-- ------------------------------------------------------------------ -->
  <!-- Verify                                                             -->
  <!-- ------------------------------------------------------------------ -->
  {#if status.baseline !== null}
    <section class="panel">
      <h2>Verify against baseline</h2>
      <p>
        <button onclick={() => check(false)} disabled={busy !== null}>Full check</button>
        <button onclick={() => check(true)} disabled={busy !== null}>Quick check</button>
      </p>
      <p class="muted">
        A quick check compares sizes and modes only. It can show that something changed, never that
        nothing did.
      </p>
      {#if verification !== null && outcome !== null}
        <p>
          <strong class="severity-{outcome.severity}" data-testid="verdict">{outcome.label}</strong>
          <span class="muted">{outcome.detail}</span>
        </p>
        {#if differences(verification).length > 0}
          <table>
            <thead><tr><th>File</th><th>Result</th><th>Meaning</th></tr></thead>
            <tbody>
              {#each differences(verification) as finding (finding.root_key + finding.path)}
                {@const copy = classificationCopy(finding.classification)}
                <tr>
                  <td><code>{finding.root_key}:{finding.path}</code></td>
                  <td class="severity-{copy.severity}">{copy.label}</td>
                  <td class="muted">{finding.detail ?? copy.detail}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {/if}
    </section>

    <!-- ---------------------------------------------------------------- -->
    <!-- Return to clean                                                  -->
    <!-- ---------------------------------------------------------------- -->
    <section class="panel">
      <h2>Return to clean</h2>
      <p class="muted">
        Removes every active Onera mod and puts back the originals Onera set aside. It never deletes
        a file Onera did not deploy, and never invents content for a damaged game file.
      </p>
      <p>
        <button onclick={previewClean} disabled={busy !== null}>Preview</button>
        {#if cleanPreview !== null}
          <button class="danger" onclick={applyClean} disabled={busy !== null}>
            Return to clean
          </button>
        {/if}
      </p>

      {#each [cleanPreview, cleanReport] as result, index (index)}
        {#if result !== null}
          {@const restored = 'restored' in result ? result.restored : result.restorable}
          <h3>{index === 0 ? 'What would happen' : 'What happened'}</h3>
          <p data-testid={index === 0 ? 'clean-preview' : 'clean-report'}>
            {restored.length} file(s)
            {index === 0 ? 'can be restored' : 'restored'} from Onera's own backups.
          </p>
          {#if result.needs_store_repair.length > 0}
            <p class="severity-warning">
              {result.needs_store_repair.length} file(s) need the store's own repair — Onera has no trusted
              copy and will not invent one.
            </p>
            <ul>
              {#each result.needs_store_repair as repair (repair.root_key + repair.path)}
                <li>
                  <code>{repair.root_key}:{repair.path}</code>
                  <span class="muted">{classificationCopy(repair.classification).label}</span>
                </li>
              {/each}
            </ul>
          {/if}
          {#if result.unknown_extras.length > 0}
            <p class="severity-warning">
              {result.unknown_extras.length} unknown extra file(s). Onera never deletes these — each one
              is your decision.
            </p>
            <ul>
              {#each result.unknown_extras as extra (extra.root_key + extra.path)}
                <li><code>{extra.root_key}:{extra.path}</code></li>
              {/each}
            </ul>
          {/if}
          {#if index === 1 && 'clean' in result}
            <p class="severity-{result.clean ? 'neutral' : 'warning'}">
              {result.clean
                ? 'The installation now matches its baseline byte for byte.'
                : 'The differences above remain, and are reported rather than acted on.'}
            </p>
          {/if}
        {/if}
      {/each}
    </section>
  {/if}
{/if}

<style>
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 1rem;
    margin: 0 0 0.75rem;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  h3 {
    font-size: 0.9rem;
    margin: 1rem 0 0.25rem;
  }
  select {
    font: inherit;
    padding: 0.3rem;
    border-radius: 6px;
    border: 1px solid var(--line);
    background: var(--panel);
    color: inherit;
  }
  ul {
    margin: 0.25rem 0;
    padding-left: 1.25rem;
  }
</style>
