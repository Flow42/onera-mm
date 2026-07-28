# Recovery behaviour

What Onera does when something goes wrong, and how to check that it works.

## On every launch

Onera reads back every operation whose state is not terminal. Each one is
presented with the choices its state allows — the state machine decides which,
so recovery cannot offer something unsafe.

| Found in state | What is true on disk                                         | Offered                          |
| -------------- | ------------------------------------------------------------ | -------------------------------- |
| `Planned`      | Nothing changed; temporary files may exist                   | Discard (cleans temporary files) |
| `Prepared`     | Backups exist, temporary files staged; **no target changed** | Continue or roll back            |
| `Committing`   | Targets may be half-swapped; the journal says which          | Continue or roll back            |
| `RollingBack`  | Undo was in progress                                         | Resume the rollback              |
| `Failed`       | Recorded and actual state disagree                           | Nothing automatic — inspect      |

```sh
onera recover              # list what was interrupted
onera recover --rollback   # undo it all
```

The desktop application shows the same list under **Recovery**.

## What survives what

**Power loss during staging.** Backups and temporary files exist; no target
changed. Rolling back deletes the temporary files. The game is untouched — a
test injects a staging failure and asserts exactly this.

**Power loss during the commit loop.** Some files are renamed, some are not. The
journal records per-file status, so rollback restores backups for committed
files and deletes the ones that had nothing before. A test fails the _second_ of
three renames and asserts that a pre-existing file survives byte-identical.

**Power loss between the rename and the journal write.** The file is in place but
recorded as `staged`. Rollback deletes it or restores its backup. The journal is
always allowed to be _ahead_ of the disk, never behind it.

**A hash mismatch after a rename.** Fatal for the operation; triggers rollback.
Onera does not accept a deployed file whose content it did not intend.

**A failed rollback.** The operation moves to `Failed` and stays there. Onera
does not retry automatically, because a failed rollback means recorded state and
disk state disagree and further automatic writes could make it worse.

**Database failure mid-operation.** The filesystem effect that was being recorded
did not happen (journal precedes effect), so the next launch sees an earlier
state and can roll back from it.

**A user deleting files behind Onera's back.** Removal reports them as
`already_missing` and carries on. Verification reports them as `Missing`.
Neither is an error.

**A user editing a deployed file.** Verification reports `Modified`. Removal
refuses to touch it without an explicit decision. An update to the same mod
reclassifies the file as `ExternallyModified` and stops the plan.

## What is deliberately not automatic

- **Repair.** `verify` reports; it never rewrites. A file the user edited on
  purpose must not be silently reverted.
- **Continuing a `Committing` operation.** Offered, but the user chooses.
- **Retrying a `Failed` rollback.**

## Manual smoke test

Run this against a real build before releasing. It exercises the same flow as
`crates/onera-app/tests/end_to_end.rs`, but against real Nexus and a real game.

**Prerequisites:** Cyberpunk 2077 installed via Steam, a Nexus account, a
running Secret Service, `p7zip-full`.

```sh
export ONERA_ROOT=$(mktemp -d)   # keep the test out of your real data
cargo build --release -p onera-cli
ONERA=./target/release/onera
```

| #   | Step                                                                        | Expected                                                     |
| --- | --------------------------------------------------------------------------- | ------------------------------------------------------------ |
| 1   | `$ONERA discover`                                                           | Cyberpunk 2077 listed as `[ok]` with its real path           |
| 2   | `$ONERA auth login` (paste key)                                             | `signed in as <you>`; input is not echoed                    |
| 3   | `secret-tool search service onera`                                          | The key is in the keyring                                    |
| 4   | `grep -r "$YOUR_KEY" "$ONERA_ROOT"`                                         | **No matches.** This is the important one                    |
| 5   | Confirm the game in the desktop app, then `$ONERA games`                    | One row with a UUID                                          |
| 6   | `$ONERA mod cyberpunk2077 107`                                              | Mod name, author and files listed                            |
| 7   | `$ONERA install --game <id> --domain cyberpunk2077 --mod-id 107`            | Dry-run preview; layout rationale shown; **nothing written** |
| 8   | `ls "<game>/bin/x64/plugins"`                                               | Unchanged — confirm step 7 really was dry                    |
| 9   | Repeat step 7 with `--apply`                                                | `installed …: N written`                                     |
| 10  | `$ONERA verify --game <id> --installation <id>`                             | `Ok: N`, exit code 0                                         |
| 11  | Edit one deployed file, re-run verify                                       | `Modified: 1`, exit code 2                                   |
| 12  | Restore the file, `$ONERA remove --game <id> --installation <id> --dry-run` | Reports deletions; **nothing removed**                       |
| 13  | Repeat without `--dry-run`                                                  | Files gone; game's own directories still present             |
| 14  | `$ONERA recover`                                                            | `no interrupted operations`                                  |
| 15  | Launch the game                                                             | Starts normally                                              |

### Conflict and restoration path

| #   | Step                                                    | Expected                                          |
| --- | ------------------------------------------------------- | ------------------------------------------------- |
| 16  | Put a file by hand where a mod will install one         | —                                                 |
| 17  | Install that mod                                        | Plan stops: `UnmanagedExisting`, refuses to apply |
| 18  | Choose "replace after backup" in the desktop app, apply | Installs; `backed_up: 1`                          |
| 19  | `$ONERA ownership --game <id> <path>`                   | Two entries: unmanaged original, then the mod     |
| 20  | Remove the mod                                          | Your original file is back, **byte for byte**     |

### Interruption path

| #   | Step                                                | Expected                                           |
| --- | --------------------------------------------------- | -------------------------------------------------- |
| 21  | Start a large install, `kill -9` during "deploying" | —                                                  |
| 22  | `$ONERA recover`                                    | One operation, state `committing` or `prepared`    |
| 23  | `$ONERA recover --rollback`                         | `rolled back`                                      |
| 24  | Inspect the game directory                          | No `.onera-tmp-*` files; pre-existing files intact |
| 25  | `$ONERA recover`                                    | `no interrupted operations`                        |

Record the results with the build's commit hash. Steps 4, 8, 12, 20 and 24 are
the ones that catch real regressions; the rest confirm the flow works at all.
