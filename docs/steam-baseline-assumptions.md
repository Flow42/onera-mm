# Steam baseline assumptions

What Onera relies on from Steam to answer "which build of this game is
installed?", how much of that answer is trustworthy, and what breaks if Valve
changes it. Companion to [`nexus-api-assumptions.md`](nexus-api-assumptions.md).

The short version: **Onera reads one file Steam wrote next to the game and
nothing else.** No Steam credentials, no running client, no local Steam IPC, no
undocumented web endpoint, no scraping. That constraint is not a limitation to
be engineered around later — it is the reason a baseline can be captured on a
machine with Steam closed, in a container, or by a user who will never give a
mod manager their store login.

## What is read

`<library>/steamapps/appmanifest_<appid>.acf`, a Valve KeyValues (VDF) text
file. Steam writes one per installed application into the `steamapps` directory
of the library the game lives in, and keeps it up to date as part of installing
and updating. `crates/onera-discovery/src/identity.rs` parses it;
`crates/onera-discovery/src/vdf.rs` is the reader.

Onera reads these keys and ignores every other one:

| Key                             | Used for                                   |
| ------------------------------- | ------------------------------------------ |
| `appid`                         | Matching an installation to a game adapter |
| `installdir`                    | Locating the game directory                |
| `name`                          | Display only                               |
| `buildid`                       | Build identity                             |
| `betakey` (see below)           | Branch identity                            |
| `InstalledDepots/<id>/manifest` | Depot identity                             |

Nothing is written back. Onera never modifies a Steam manifest.

## Locating the manifest

The `GameStore` adapter finds an installation's manifest **by layout, not by
Steam root**. A Steam-managed game always sits at
`<library>/steamapps/common/<installdir>`, so the manifest directory is two
levels up from the game directory, and the manifest is the one in it whose
`installdir` names this directory.

That single rule covers every layout Onera supports, because they differ only in
the prefix in front of `steamapps`, which this never looks at:

| Layout        | Example prefix                                            |
| ------------- | --------------------------------------------------------- |
| Native        | `~/.local/share/Steam`, `~/.steam/steam`, `~/.steam/root` |
| Flatpak       | `~/.var/app/com.valvesoftware.Steam/.local/share/Steam`   |
| Extra library | any path listed in `steamapps/libraryfolders.vdf`         |

Discovery (`steam::find_steam_installs`) still enumerates those prefixes,
because finding games in the first place needs a starting point. Identity
lookup for an already-registered installation does not.

## What is trustworthy, and what is best effort

This is the distinction the rest of Milestone 2 rests on. A baseline is stamped
with this identity, and staleness detection compares it — so a value that is
confidently wrong is worse than one that is absent.

### Trustworthy

- **The manifest path.** Onera opened that file; it is recorded verbatim for
  diagnostics and is never used as an identifier.
- **The AppID.** Cross-checked between the file name and the `appid` key. Steam
  indexes `steamapps/` by file name, so a manifest whose body names a different
  application is rejected outright rather than reconciled — either value could
  attach a build identity to the wrong game.
- **The `installdir`, and therefore the manifest-to-directory association.**

### Best effort

Present when Steam recorded them, absent otherwise. **Absent is `None`, never a
default, a placeholder or a plausible-looking value.** A fabricated identifier
would compare equal to the next fabricated one and report a changed build as
unchanged, which is the exact failure this design exists to prevent.

- **`buildid`.** Absent from manifests Steam created but has not finished
  filling in, where it appears as the placeholder `0`. Onera drops `0` and any
  non-numeric value: two half-written installations must not compare `Same`.
- **The branch/beta key.** Steam records it in `MountedConfig`, `UserConfig`, or
  occasionally at the top level. **`MountedConfig` wins**, because after
  switching branches but before downloading, `UserConfig` names the branch the
  user asked for while `MountedConfig` names the content actually on disk — and
  a baseline describes disk. An empty key means the default branch, which Onera
  records as no branch rather than inventing the string `public`.
- **Installed depots and their manifest IDs**, from `InstalledDepots`. A depot
  entry missing either identifier, or carrying a non-numeric or placeholder one,
  is dropped rather than half-recorded. Dropping entries can only weaken an
  identity toward `Unknown`; it can never strengthen it into a wrong `Same`.
  `SharedDepots` is deliberately **not** read: its values are application IDs,
  not manifest IDs, and reading them as manifests would invent identity.

### Not available at all

- **DLC ownership.** `GameStore::owned_dlc` returns `StoreCapability::Unknown`.
  `InstalledDepots` lists depots that are _installed_, which is neither
  ownership nor a complete list, and the ownership APIs Valve does publish
  require credentials Onera will not ask for. Returning an empty list would let
  a dependency solver conclude the user owns no DLC.
- **The expected file set for a build.** See below.

## Why there is no authoritative manifest

Valve documents depot manifests as carrying file paths, sizes, flags and SHA-1
hashes, and documents builds and depots as separately identified. What Valve
does **not** publish is a supported consumer API for retrieving the complete
expected manifest for an installed build. The paths that exist require either
partner credentials or reverse-engineered client protocols.

So Onera's first release captures a **local baseline** instead: the user runs
Steam's own _Verify Installed Files_, confirms it finished, and Onera hashes what
is on disk and stamps the result with the build identity above. That produces
`BaselineSource::StoreVerifiedCapture`, and the wording matters —

> The capture is a local observation stamped with Steam build identity, not a
> claim that Steam independently attested every byte.

`onera_core::ports::GameManifestProvider` exists for the day that changes.
`onera_discovery::store::SteamManifestProvider` implements it and reports
`ManifestAvailability::Unsupported`, which the baseline domain already
distinguishes from "we asked and it failed". The implementation boundary is
real; the capability is honestly reported as absent. If Valve ships a supported
consumer API, that one type changes and the baseline domain, the scanner and the
UI do not.

## Consequences for staleness

`StoreBuildIdentity::compare` is equality over opaque strings — never ordering,
never parsing. It follows that:

- A different `buildid`, branch or depot manifest ID means **changed**, whether
  Steam rolled the game forward or the user rolled it back.
- If either side lacks both a `buildid` and any depot, the answer is
  **`Unknown`**, not `Same`. A game whose manifest carries no usable identity
  can still be given a baseline; that baseline is simply shown as unverifiable
  rather than fresh.
- A manual (non-Steam) installation gets `Unknown` from this adapter even if its
  directory happens to sit inside a Steam library. Onera did not learn the path
  from Steam and will not assert a Steam build for it; its baseline is a
  clearly labelled local snapshot.

## What breaks if Valve changes something

| Change                            | Effect                                                                  |
| --------------------------------- | ----------------------------------------------------------------------- |
| Manifest moves or is renamed      | Identity becomes `Unknown`; baselines still capture, shown unverifiable |
| `.acf` stops being KeyValues text | Same — the parse fails and nothing is invented                          |
| `buildid` becomes non-numeric     | Dropped as malformed; depots alone still detect changes                 |
| A depot key is renamed            | That depot is dropped; remaining identity still compares                |
| A supported manifest API appears  | Implement `GameManifestProvider`; nothing else changes                  |

Every one of these degrades to `Unknown`. None of them can produce a false
`Same`, which is the property worth protecting: a stale baseline that presents
itself as fresh would let Onera compare a modded game against the wrong clean
state.

## Revalidation

These assumptions were recorded at the start of Milestone 2. Re-check them at
the start of Milestone 4 and before any release that changes baseline
behaviour, against:

- <https://partner.steamgames.com/doc/store/application/builds>
- <https://partner.steamgames.com/doc/store/application/depots>
- <https://partner.steamgames.com/doc/sdk/uploading>

The fixtures in `crates/onera-discovery/tests/fixtures/steam/` are the
executable form of this document. A manifest shape Valve introduces should
arrive here as a new fixture first.
