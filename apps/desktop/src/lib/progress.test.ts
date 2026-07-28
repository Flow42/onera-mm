/**
 * Progress reduction. Pure, so every event ordering can be checked.
 */
import { describe, expect, it } from 'vitest';
import { fraction, initial, reduce } from './progress.svelte';
import type { ProgressEvent } from './bridge';

const started = (stage: string, total: number | null): ProgressEvent => ({
  type: 'started',
  stage,
  total,
});

describe('reduce', () => {
  it('starts a stage with reset counters', () => {
    let state = initial();
    state = reduce(state, started('downloading', 100));
    expect(state.stage).toBe('downloading');
    expect(state.completed).toBe(0);
    expect(state.total).toBe(100);
  });

  it('accumulates progress within a stage', () => {
    let state = reduce(initial(), started('downloading', 100));
    state = reduce(state, {
      type: 'advanced',
      stage: 'downloading',
      completed: 40,
      total: 100,
      detail: 'mod.zip',
    });
    expect(state.completed).toBe(40);
    expect(state.detail).toBe('mod.zip');
    expect(fraction(state)).toBeCloseTo(0.4);
  });

  it('resets counters when a new stage begins', () => {
    let state = reduce(initial(), started('downloading', 100));
    state = reduce(state, {
      type: 'advanced',
      stage: 'downloading',
      completed: 100,
      total: 100,
      detail: null,
    });
    state = reduce(state, started('extracting', 20));
    expect(state.completed).toBe(0);
    expect(state.total).toBe(20);
    expect(state.finished).toBe(false);
  });

  it('collects warnings without losing earlier ones', () => {
    let state = initial();
    state = reduce(state, { type: 'warning', message: 'first' });
    state = reduce(state, { type: 'warning', message: 'second' });
    expect(state.warnings).toEqual(['first', 'second']);
  });

  it('records success and failure distinctly', () => {
    const ok = reduce(initial(), { type: 'finished', stage: 'deploying', success: true });
    expect(ok.finished).toBe(true);
    expect(ok.succeeded).toBe(true);

    const failed = reduce(initial(), { type: 'finished', stage: 'deploying', success: false });
    expect(failed.finished).toBe(true);
    expect(failed.succeeded).toBe(false);
  });

  it('never mutates the state it is given', () => {
    const before = initial();
    const snapshot = structuredClone(before);
    reduce(before, started('downloading', 10));
    expect(before).toEqual(snapshot);
  });
});

describe('fraction', () => {
  it('is null when the total is unknown', () => {
    // Common and normal: a download's byte count is often not declared, and the
    // UI must show an indeterminate bar rather than inventing a number.
    expect(fraction({ ...initial(), completed: 5, total: null })).toBeNull();
    expect(fraction({ ...initial(), completed: 5, total: 0 })).toBeNull();
  });

  it('clamps to one when more arrives than was declared', () => {
    expect(fraction({ ...initial(), completed: 150, total: 100 })).toBe(1);
  });
});
