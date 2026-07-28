# The operation state machine

Every mutation of a game directory is a journaled **operation**. The journal is
the sole source of truth for crash recovery: on startup Onera reads back every
operation that is not terminal and decides, purely from recorded state, what can
be done about it.

## States

```text
            ┌──────────┐
            │ Planned  │  plan persisted; nothing on disk has changed
            └────┬─────┘
                 │ prepare
            ┌────▼─────┐
            │ Prepared │  backups written, temporary files staged and hashed
            └────┬─────┘
                 │ commit
            ┌────▼─────┐
            │Committing│  renames in flight — the only risky window
            └────┬─────┘
                 │ verify + record
            ┌────▼─────┐
            │ Complete │  terminal
            └──────────┘

  Planned ────abort────► RolledBack   (terminal; nothing to undo)
  Prepared ───abort────► RollingBack ──► RolledBack
  Committing ──fail────► RollingBack ──► RolledBack
  RollingBack ──fail───► Failed       (terminal; needs the user)
```

`OperationState::can_transition_to` is the authority, and a test asserts that
terminal states have no outgoing transitions and that no state can be skipped.
`Database::set_state` re-checks the transition inside a transaction, so two
concurrent callers cannot both observe the old state and both advance from it.

## What each state guarantees

| State         | On disk                                                                                            | Recovery offers                                   |
| ------------- | -------------------------------------------------------------------------------------------------- | ------------------------------------------------- |
| `Planned`     | Nothing has changed. Temporary files _may_ exist, because staging begins before the state advances | Discard the plan (after cleaning temporary files) |
| `Prepared`    | Backups exist; temporary files are staged and hash-verified; **no target has changed**             | Continue or roll back                             |
| `Committing`  | Targets may be half-swapped; the journal records which are done                                    | Continue or roll back                             |
| `Complete`    | Everything deployed, verified and recorded                                                         | Nothing                                           |
| `RollingBack` | Undo in progress                                                                                   | Resume the rollback                               |
| `RolledBack`  | Back to the pre-operation state                                                                    | Nothing                                           |
| `Failed`      | Recorded state and disk state disagree                                                             | Nothing automatic — the user must inspect         |

A test asserts that **every** non-terminal state offers some recovery, so adding
a state without a recovery path fails the build.

## The per-file journal

`operation_files` holds one row per planned file, written _before_ the file is
touched and updated after each atomic step:

| Status        | Meaning                                                             | Undo                                                      |
| ------------- | ------------------------------------------------------------------- | --------------------------------------------------------- |
| `pending`     | Recorded; nothing done                                              | Nothing                                                   |
| `staged`      | Backup taken (if needed), temporary file written and hash-verified  | Delete the temporary file                                 |
| `committed`   | Renamed into place and re-verified                                  | Restore the backup, or delete if nothing was there before |
| `skipped`     | Deliberately not applied (skip, adopt, or shared identical content) | Nothing                                                   |
| `rolled_back` | Undone                                                              | Nothing                                                   |

Each row stores the **resolved absolute path** as well as the root key and
relative path. Recovery runs after a crash, when the game adapter may not even
be loadable; it must not have to re-derive deployment roots to undo a write.

## Ordering: why this works

Writes to the journal always precede the filesystem effect and are confirmed
after it. A crash can therefore leave the journal **ahead** of the disk — a step
recorded but not performed — which recovery redoes or undoes idempotently. It
can never leave the journal **behind** the disk, which recovery could not see.

Every undo step is idempotent: restoring an already-restored backup, deleting an
already-deleted temporary file. That is what lets a rollback itself be
interrupted and resumed.

## Why cancellation stops before the commit loop

`CancelToken` is checked at every step up to and including the transition to
`Committing`, and then not again until the loop finishes. Once renames have
started, stopping halfway is strictly worse than finishing: a completed install
can be removed cleanly through the normal path, whereas a half-applied one needs
recovery. Cancellation is honoured at the next operation boundary instead.

## Serialization

Deployments are serialized **per game installation** by `GameLocks`. Two
different games install concurrently; two installs into the same game do not,
because interleaved renames would produce a provider stack matching neither plan.
