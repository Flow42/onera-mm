/**
 * The preview view-model. These are the rules that decide whether the Install
 * button is offered, so they are worth testing exhaustively.
 */
import { describe, expect, it } from 'vitest';
import {
  canApply,
  describe as describeClassification,
  formatBytes,
  summarise,
  unresolved,
} from './plan-view';
import type { Classification, InstallPlanView, PlannedFileView } from './types';

function file(
  target: string,
  classification: Classification,
  decision: string | null = null,
): PlannedFileView {
  return {
    source: target,
    target,
    classification,
    action: 'write',
    existing_hash: null,
    notes: [],
    decision,
  };
}

function plan(files: PlannedFileView[], ready = true): InstallPlanView {
  return {
    operation_id: 'op',
    installation_id: 'inst',
    mod_name: 'A mod',
    layout_rationale: 'stripped 1 wrapper directory',
    ignored: 0,
    rejected: [],
    ready,
    bytes_to_write: 0,
    files,
  };
}

describe('describe', () => {
  it('covers every classification the core can emit', () => {
    const all: Classification[] = [
      'create',
      'identical',
      'replace_previous_release',
      'conflict_with_other_mod',
      'unmanaged_existing',
      'externally_modified',
      'invalid_target',
      'skipped_by_rule',
    ];
    for (const classification of all) {
      const copy = describeClassification(classification);
      expect(copy.label, classification).toBeTruthy();
      expect(copy.detail, classification).toBeTruthy();
    }
  });

  it('marks exactly the three always-ask classifications as needing a decision', () => {
    const needsDecision = (c: Classification) => describeClassification(c).needsDecision;
    expect(needsDecision('conflict_with_other_mod')).toBe(true);
    expect(needsDecision('unmanaged_existing')).toBe(true);
    expect(needsDecision('externally_modified')).toBe(true);
    expect(needsDecision('create')).toBe(false);
    expect(needsDecision('identical')).toBe(false);
    expect(needsDecision('replace_previous_release')).toBe(false);
  });

  it('fails safe on an unknown classification from a newer backend', () => {
    const copy = describeClassification('something_new');
    expect(copy.needsDecision).toBe(true);
    expect(copy.severity).toBe('warning');
  });
});

describe('unresolved and canApply', () => {
  it('offers Install for a clean plan', () => {
    const p = plan([file('game:a', 'create'), file('game:b', 'identical')]);
    expect(unresolved(p)).toHaveLength(0);
    expect(canApply(p)).toBe(true);
  });

  it('withholds Install while a conflict is open', () => {
    const p = plan([file('game:a', 'create'), file('game:b', 'conflict_with_other_mod')], false);
    expect(unresolved(p)).toHaveLength(1);
    expect(canApply(p)).toBe(false);
  });

  it('counts a decided conflict as resolved', () => {
    const p = plan([file('game:b', 'unmanaged_existing', 'replace_after_backup')]);
    expect(unresolved(p)).toHaveLength(0);
    expect(canApply(p)).toBe(true);
  });

  it('still withholds Install when the backend says the plan is not ready', () => {
    // Belt and braces: the frontend never offers the action the backend would
    // refuse, even if its own checks pass.
    const p = plan([file('game:a', 'create')], false);
    expect(unresolved(p)).toHaveLength(0);
    expect(canApply(p)).toBe(false);
  });
});

describe('summarise', () => {
  it('counts classifications, most common first', () => {
    const p = plan([
      file('game:a', 'create'),
      file('game:b', 'create'),
      file('game:c', 'identical'),
    ]);
    expect(summarise(p)).toEqual([
      { classification: 'create', count: 2 },
      { classification: 'identical', count: 1 },
    ]);
  });

  it('handles an empty plan', () => {
    expect(summarise(plan([]))).toEqual([]);
  });
});

describe('formatBytes', () => {
  it('scales through the binary units', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1024)).toBe('1.0 KiB');
    expect(formatBytes(1024 * 1024 * 3.5)).toBe('3.5 MiB');
    expect(formatBytes(1024 ** 4)).toBe('1.0 TiB');
  });

  it('refuses to render nonsense as a number', () => {
    expect(formatBytes(-1)).toBe('—');
    expect(formatBytes(Number.NaN)).toBe('—');
  });
});
