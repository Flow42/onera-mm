import { describe, expect, it } from 'vitest';
import {
  bytes,
  buildLabel,
  classificationCopy,
  differences,
  freshnessCopy,
  hasDifferences,
  isClean,
  sourceCopy,
  verdict,
} from './baseline-view';
import type {
  BaselineVerification,
  FileClassification,
  FindingCounts,
  GameBaseline,
  StoreBuildIdentity,
} from './types';

const identity = (build: string | null): StoreBuildIdentity => ({
  store: 'steam',
  app_id: '1091500',
  build_id: build,
  branch: null,
  depots: [{ depot_id: '1091501', manifest_id: '77' }],
  manifest_path: '/games/steamapps/appmanifest_1091500.acf',
  observed_at: '2026-09-01T10:00:00Z',
});

const baseline: GameBaseline = {
  id: '3f2b',
  local_game_id: '9a1c',
  source: 'store_verified_capture',
  build_identity: identity('18320471'),
  adapter_id: 'cyberpunk2077',
  reported_version: '2.21',
  status: 'current',
  captured_at: '2026-09-01T10:04:12Z',
  scope_fingerprint: 'b3',
  file_count: 41233,
  total_bytes: 71234567890,
};

const counts = (over: Partial<FindingCounts> = {}): FindingCounts => ({
  matching: 41233,
  modified: 0,
  missing: 0,
  extra_managed: 0,
  extra_unknown: 0,
  unreadable: 0,
  special: 0,
  ...over,
});

const verification = (over: Partial<BaselineVerification> = {}): BaselineVerification => ({
  baseline_id: '3f2b',
  scan_run_id: '77a',
  state: 'completed',
  evidence: 'content_hashed',
  scope_fingerprint: 'b3',
  findings: [],
  counts: counts(),
  verified_at: '2026-09-02T09:12:00Z',
  ...over,
});

describe('freshness', () => {
  it('never presents an unknown result as fresh', () => {
    const unknown = freshnessCopy({
      kind: 'unknown',
      reason: 'the store did not expose a comparable build identity',
    });
    expect(unknown.label).not.toMatch(/fresh/i);
    expect(unknown.severity).toBe('warning');
    expect(unknown.detail).toMatch(/not the same as/i);
  });

  it('never presents a missing baseline as fresh', () => {
    expect(freshnessCopy({ kind: 'none' }).label).not.toMatch(/fresh/i);
  });

  it('names both builds when the game changed', () => {
    const stale = freshnessCopy({
      kind: 'stale',
      captured: identity('18320471'),
      observed: identity('18400000'),
    });
    expect(stale.severity).toBe('warning');
    expect(stale.detail).toContain('18320471');
    expect(stale.detail).toContain('18400000');
  });

  it('fails safe on a freshness state from a newer backend', () => {
    const copy = freshnessCopy({ kind: 'reticulated' } as never);
    expect(copy.severity).toBe('warning');
    expect(copy.label).not.toMatch(/fresh/i);
  });

  it('says unknown rather than inventing a build label', () => {
    expect(buildLabel(null)).toBe('unknown');
    expect(buildLabel({ ...identity(null), depots: [] })).toBe('unknown');
    expect(buildLabel(identity(null))).toContain('depot');
    expect(buildLabel(identity('18320471'))).toContain('18320471');
  });
});

describe('source labelling', () => {
  it('labels a local snapshot as not store-verified', () => {
    const copy = sourceCopy('local_snapshot');
    expect(copy.label).toMatch(/not store-verified/i);
    expect(copy.detail).toMatch(/not that they were ever correct/i);
  });

  it('does not overstate what a store-verified capture proves', () => {
    expect(sourceCopy('store_verified_capture').detail).toMatch(/not a claim/i);
  });
});

describe('clean verdicts', () => {
  it('is clean only for a completed, content-hashed, in-scope, difference-free scan', () => {
    expect(isClean(verification(), baseline)).toBe(true);
    expect(isClean(verification({ evidence: 'metadata_only' }), baseline)).toBe(false);
    expect(isClean(verification({ state: 'cancelled' }), baseline)).toBe(false);
    expect(isClean(verification({ scope_fingerprint: 'other' }), baseline)).toBe(false);
    expect(isClean(verification({ counts: counts({ extra_unknown: 1 }) }), baseline)).toBe(false);
    expect(isClean(verification(), null)).toBe(false);
  });

  it('never calls a quick scan clean, however good its counts look', () => {
    const quick = verification({ evidence: 'metadata_only' });
    expect(isClean(quick, baseline)).toBe(false);
    const copy = verdict(quick, baseline);
    expect(copy.label).not.toBe('Clean');
    expect(copy.detail).toMatch(/never that nothing did/i);
  });

  it('explains an incomplete scan rather than reporting its partial counts as clean', () => {
    const copy = verdict(verification({ state: 'cancelled' }), baseline);
    expect(copy.label).toBe('Incomplete');
    expect(copy.severity).toBe('warning');
  });

  it('refuses to compare a scan whose scope changed', () => {
    const copy = verdict(verification({ scope_fingerprint: 'narrower' }), baseline);
    expect(copy.label).toBe('Scope changed');
  });

  it('counts every non-matching class as a difference', () => {
    for (const key of [
      'modified',
      'missing',
      'extra_managed',
      'extra_unknown',
      'unreadable',
      'special',
    ] as const) {
      expect(hasDifferences(verification({ counts: counts({ [key]: 1 }) }))).toBe(true);
    }
    expect(hasDifferences(verification())).toBe(false);
  });
});

describe('classifications', () => {
  it('requires a decision for extras, unreadable paths and links', () => {
    for (const kind of ['extra_unknown', 'unreadable', 'special_file'] as FileClassification[]) {
      expect(classificationCopy(kind).needsDecision).toBe(true);
    }
    for (const kind of [
      'matching',
      'modified',
      'missing',
      'extra_managed',
    ] as FileClassification[]) {
      expect(classificationCopy(kind).needsDecision).toBe(false);
    }
  });

  it('fails safe on a classification from a newer backend', () => {
    const copy = classificationCopy('reticulated' as FileClassification);
    expect(copy.needsDecision).toBe(true);
    expect(copy.severity).toBe('danger');
  });

  it('says an unknown extra is never deleted for you', () => {
    expect(classificationCopy('extra_unknown').detail).toMatch(/never deletes it for you/i);
  });

  it('hides matching files from the differences table', () => {
    const scan = verification({
      findings: [
        {
          root_key: 'game',
          path: 'a',
          classification: 'matching',
          expected: null,
          observed: null,
          detail: null,
        },
        {
          root_key: 'game',
          path: 'b',
          classification: 'extra_unknown',
          expected: null,
          observed: null,
          detail: null,
        },
      ],
    });
    expect(differences(scan).map((f) => f.path)).toEqual(['b']);
  });
});

describe('byte formatting', () => {
  it('scales without inventing precision', () => {
    expect(bytes(512)).toBe('512 B');
    expect(bytes(71234567890)).toBe('71.2 GB');
  });
});
