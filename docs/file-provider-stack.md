# The file-provider stack

This is the idea the rest of Onera is built around, so it is worth its own page.

## The problem

Two mods want to write `archive/pc/mod/vehicles.archive`. A load-order-based
manager writes the winner and forgets the loser. Uninstall the winner and the
file is simply gone — even though another installed mod still expects to provide
it. Uninstall a mod that overwrote a _vanilla_ file and the game is left with a
hole.

## The model

Every deployed relative path, under every deployment root, owns an **ordered
stack** of providers rather than a single owner.

```text
archive/pc/mod/vehicles.archive
  ┌────────────────────────────────────────┐
  │ 2. Mod B, installation 8f3c…  ← on disk│   top    = what is deployed now
  ├────────────────────────────────────────┤
  │ 1. Mod A, installation 41ab…           │
  ├────────────────────────────────────────┤
  │ 0. Unmanaged original (backup 9d02…)   │   bottom = what was there first
  └────────────────────────────────────────┘
```

An entry is either an **installation** or an **unmanaged backup**, and carries
the BLAKE3 hash and size of the content _that provider supplies_. The top of the
stack is what is on disk.

Removing a provider is then a local, obvious operation:

| Removed                                       | Result                                         |
| --------------------------------------------- | ---------------------------------------------- |
| Top entry, and something is beneath it        | Restore the entry beneath                      |
| Top entry, and it is the last one             | Delete the file                                |
| Top entry, but the next one has the same hash | **Do nothing** — the bytes are already correct |
| A buried entry                                | Do nothing on disk; just drop the claim        |

All four cases are `ProviderStack::remove_installation`, and all four are tested
in `crates/onera-core/src/domain/provider_stack.rs`.

## What this buys

**Cross-mod restoration.** Remove Mod B and Mod A's file comes back, because Mod
A's entry never left the stack.

**Unmanaged restoration.** Remove the last mod and the pre-Onera original comes
back from its backup, byte for byte. Onera's own tests assert byte equality, not
just "a file exists".

**Downgrades.** Installing an older release of the same mod is just a new entry
whose content happens to be older bytes. Nothing special-cases "downgrade".

**Shared identical files.** Two mods that ship byte-identical content both
appear on the stack, and neither removal rewrites anything. This is why
installing a mod that bundles a vanilla file causes no prompt and no write.

**Re-installation is idempotent.** Pushing an installation that is already on the
stack updates its entry in place and moves it to the top, rather than stacking a
duplicate. Without that, installing twice would require removing twice.

## Storage

```sql
deployed_files            -- one row per (game, root, relative path)
deployed_file_providers   -- one row per stack entry, ordered by `position`
```

`position` 0 is the bottom. Writing a stack deletes and re-inserts every row for
that path inside one transaction, which keeps positions dense and makes a
partially written stack impossible.

A `CHECK` constraint enforces that an entry names exactly one provider: an
installation row has `installation_id` and no `backup_id`, an unmanaged row has
the reverse. There is a test asserting the constraint exists, because losing it
would let a row exist that no code path could interpret.

`file_provider_history` is an append-only audit trail alongside it, recording
every ownership change with the operation that caused it. The stack answers
"what happens if I remove this?"; the history answers "how did this file get
like this?".

## Restoring content, not just records

Restoring the entry beneath the top needs its _bytes_, not just its hash.
Because backups are content-addressed by BLAKE3, `BackupStore::path_of_hash`
finds them from the hash alone — no path bookkeeping, and no dependence on which
operation happened to take the backup. Two mods that both overwrote the same
vanilla file share a single stored blob, and it is only reclaimed when the last
row referencing it goes away.

## Version comparison

Stack entries carry hashes, never version strings. Onera compares versions only
within one mod lineage and only by publication date; `Release::is_newer_than`
panics outright if handed two releases of different mods. Mod authors use
mutually incompatible version schemes, and a cross-mod comparison is meaningless.
