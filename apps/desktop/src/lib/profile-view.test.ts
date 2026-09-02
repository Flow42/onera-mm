import { describe, expect, it } from 'vitest';
import {
  activationCopy,
  canActivate,
  dependencyHealthCopy,
  dependencyOutcomeCopy,
  evidenceNotices,
  formatTarget,
  mutationStepCopy,
  signedPriority,
} from './profile-view';
import type {
  DependencyEvidence,
  ProfileActivation,
  ProfileActivationPreview,
  ResolutionResult,
} from './types';

const evidence = (over: Partial<DependencyEvidence> = {}): DependencyEvidence => ({
  fresh: 1,
  cached: 0,
  stale: 0,
  unavailable: 0,
  unsupported: 0,
  unknown_dlc: 0,
  ...over,
});

const resolution = (kind: ResolutionResult['outcome']['kind']): ResolutionResult => ({
  outcome: { kind } as ResolutionResult['outcome'],
  health: [],
  evidence: evidence(),
});

const preview = (over: Partial<ProfileActivationPreview> = {}): ProfileActivationPreview => ({
  from_profile_id: 'old',
  to_profile_id: 'new',
  plan: { steps: [], conflicts: [] },
  downloads: [],
  dependency: resolution('compatible'),
  baseline_freshness: { kind: 'fresh' },
  bytes_to_write: 0,
  ready: true,
  blockers: [],
  fingerprint: 'b3:preview',
  ...over,
});

describe('dependency presentation', () => {
  it('keeps unknown and unavailable distinct and never labels either satisfied', () => {
    expect(dependencyHealthCopy('unknown').label).toBe('Unknown');
    expect(dependencyHealthCopy('unavailable').label).toBe('Unavailable');
    expect(dependencyHealthCopy('unavailable').detail).toMatch(/does not mean no dependencies/i);
    expect(dependencyHealthCopy('future_state').severity).toBe('warning');
  });

  it('reports cached, stale, unavailable, unsupported and unknown DLC independently', () => {
    const notices = evidenceNotices(
      evidence({ cached: 1, stale: 2, unavailable: 3, unsupported: 4, unknown_dlc: 5 }),
    );
    expect(notices.map((notice) => notice.label)).toEqual([
      'Cached dependency data',
      'Stale dependency data',
      'Dependency data unavailable',
      'Dependencies unsupported',
      'DLC ownership unknown',
    ]);
  });

  it('never presents an unknown outcome as compatible or actionable', () => {
    const copy = dependencyOutcomeCopy(resolution('unknown'));
    expect(copy.label).not.toMatch(/^compatible$/i);
    expect(copy.severity).toBe('warning');
  });
});

describe('activation safety', () => {
  it('requires backend readiness, no blockers, and known satisfied member health', () => {
    expect(canActivate(preview())).toBe(true);
    expect(canActivate(preview({ ready: false }))).toBe(false);
    expect(canActivate(preview({ blockers: [{ kind: 'cross_mod_conflict' }] }))).toBe(false);
    expect(canActivate(preview({ dependency: resolution('unknown') }))).toBe(false);
    expect(
      canActivate(
        preview({
          dependency: {
            ...resolution('compatible'),
            health: [{ profile_member_id: 'm', health: 'unknown', unsatisfied: [] }],
          },
        }),
      ),
    ).toBe(false);
  });

  it('labels unknown mutation kinds as unsafe', () => {
    expect(mutationStepCopy({ kind: 'future_step', target: 'game:a' }).severity).toBe('warning');
    expect(
      mutationStepCopy({
        kind: 'write',
        target: 'game:a',
        provider: {
          provider: { kind: 'unmanaged_backup', backup_id: 'backup-1' },
          hash: 'blake3:a',
          size: 1,
        },
        expected_previous: 'blake3:b',
      }).label,
    ).toBe('Restore covered file');
    expect(formatTarget({ root_key: 'game', path: 'a/b' })).toBe('game:a/b');
  });

  it('accepts the complete signed i32 priority range only', () => {
    expect(signedPriority('-12')).toBe(-12);
    expect(signedPriority('0')).toBe(0);
    expect(signedPriority('2147483647')).toBe(2_147_483_647);
    expect(signedPriority('-2147483648')).toBe(-2_147_483_648);
    expect(signedPriority('')).toBeNull();
    expect(signedPriority('1.5')).toBeNull();
    expect(signedPriority('2147483648')).toBeNull();
  });

  it('does not report rollback or recovery failure as success', () => {
    const record = (state: ProfileActivation['state']): ProfileActivation => ({
      from_profile_id: 'old',
      to_profile_id: 'new',
      operation_id: 'op',
      state,
      started_at: '2026-09-02T10:00:00Z',
      finished_at: '2026-09-02T10:01:00Z',
      error: null,
    });
    expect(activationCopy(record('applied')).label).toBe('Profile activated');
    expect(activationCopy(record('rolled_back')).label).toBe('Activation rolled back');
    expect(activationCopy(record('failed')).label).toBe('Recovery required');
  });
});
