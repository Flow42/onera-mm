/**
 * Progress state for long-running operations.
 *
 * A single store rather than per-component state, because the same operation is
 * visible from several views at once (the downloads list, the install preview
 * and the status bar) and they must not disagree.
 *
 * Cancellation is cooperative on the Rust side, so requesting it here only sets
 * a flag; the operation ends when the core reaches its next safe point.
 */

import type { ProgressEvent } from './bridge';

export interface OperationProgress {
  stage: string;
  completed: number;
  total: number | null;
  detail: string | null;
  warnings: string[];
  finished: boolean;
  succeeded: boolean;
  cancelRequested: boolean;
}

/** A fresh, idle progress record. */
export function initial(): OperationProgress {
  return {
    stage: 'idle',
    completed: 0,
    total: null,
    detail: null,
    warnings: [],
    finished: false,
    succeeded: false,
    cancelRequested: false,
  };
}

/**
 * Fold one event into the current state.
 *
 * Pure, so the reducer can be tested exhaustively without a running backend.
 *
 * @param state - Current state.
 * @param event - Incoming event.
 * @returns The new state.
 */
export function reduce(state: OperationProgress, event: ProgressEvent): OperationProgress {
  switch (event.type) {
    case 'started':
      // A new stage resets the counters: totals are per stage, not per operation.
      return { ...state, stage: event.stage, completed: 0, total: event.total, finished: false };
    case 'advanced':
      return {
        ...state,
        stage: event.stage,
        completed: event.completed,
        total: event.total,
        detail: event.detail,
      };
    case 'warning':
      return { ...state, warnings: [...state.warnings, event.message] };
    case 'finished':
      return { ...state, stage: event.stage, finished: true, succeeded: event.success };
    default:
      return state;
  }
}

/**
 * Fraction complete, or `null` when the total is unknown.
 *
 * A null total is common and normal — an archive's entry count is known, a
 * download's byte count often is not — so the UI shows an indeterminate bar
 * rather than inventing a number.
 */
export function fraction(state: OperationProgress): number | null {
  if (state.total === null || state.total <= 0) {
    return null;
  }
  return Math.min(1, state.completed / state.total);
}
