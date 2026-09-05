# Writing a game adapter

A game adapter is the only place in Onera that knows anything about a particular
game. Adding one touches exactly two files: a new module in `onera-games`, and
one line in `all_adapters()`. The installer, the planner, the downloader and
every provider client stay untouched.

## The trait

```rust
pub trait GameAdapter: Send + Sync {
    fn id(&self) -> &str;                     // stable slug, e.g. "cyberpunk2077"
    fn display_name(&self) -> &str;
    fn provider_slugs(&self) -> &[&str];      // Nexus domain names it claims
    fn steam_app_ids(&self) -> &[u32];

    fn validate_install(&self, install_root: &Path) -> InstallValidation;
    fn deploy_roots(&self, install: &LocalGameInstall) -> Result<Vec<DeployRoot>>;
    fn resolve_layout(&self, manifest: &ArchiveManifest) -> Result<LayoutResolution>;
    fn validate_target(&self, target: &TargetLocation) -> Result<()>;

    // Both have defaults; see step 6.
    fn baseline_roots(&self, install: &LocalGameInstall) -> Result<Vec<BaselineRoot>>;
    fn baseline_exclusions(&self) -> Vec<BaselineExclusion>;
}
```

Adapters never write anything. They read for validation and they answer
questions; the planner decides what happens.

## 1. Identity

`steam_app_ids` is what discovery matches against, `provider_slugs` is what the
provider catalogue is matched against. A test asserts that no two adapters share
an id or a Steam app id, because a collision would silently route installs to
the wrong game.

## 2. `validate_install`

Pick a marker that identifies the game across **every** distribution — Steam,
GOG, Epic, Heroic. The executable is usually the only thing that qualifies.

```rust
fn validate_install(&self, root: &Path) -> InstallValidation {
    if !root.join("bin/x64/Cyberpunk2077.exe").is_file() {
        return InstallValidation::invalid("bin/x64/Cyberpunk2077.exe is missing");
    }
    InstallValidation { valid: true, reported_version: read_version(root), findings }
}
```

`findings` is for things worth telling the user without failing: "RED4ext is
present", "archive/pc/content is missing; this may be an incomplete install".

`reported_version` is stored **verbatim**. Do not normalize it.

## 3. `deploy_roots`

Model locations separately, because on Linux they genuinely are:

| Kind           | Typically                                 |
| -------------- | ----------------------------------------- |
| `GameInstall`  | `…/steamapps/common/<Game>`               |
| `CompatPrefix` | `…/steamapps/compatdata/<appid>/pfx`      |
| `UserData`     | `…/pfx/drive_c/users/steamuser/Documents` |
| `Auxiliary`    | Anything else the game needs              |

Return a `root_key` per root. Plans reference roots by key, never by absolute
path, which is what makes a plan portable and replayable.

## 4. `resolve_layout` — the interesting one

Map the archive's contents onto deployment roots. Two rules:

**Unwrap cosmetic directories.** `My Cool Mod v1.2/archive/pc/mod/x.archive`
must deploy to `archive/pc/mod/x.archive`. Cyberpunk's adapter tries wrapper
depths 0 to 4 and keeps the ones that produce a valid layout.

**When more than one reading is plausible, refuse.** Return
`CoreError::AmbiguousLayout` with a message explaining the ambiguity. The caller
asks the user. Guessing wrong scatters files through a game directory, and the
user has no way to know it happened.

```rust
match candidates.len() {
    0 => Err(CoreError::AmbiguousLayout("no recognized directory found".into())),
    1 => Ok(build_resolution(&paths, candidates[0])),
    _ => Err(CoreError::AmbiguousLayout(format!("readable {} ways", candidates.len()))),
}
```

Documentation and images go in `ignored`, not `mappings`. Deploying a readme into
a game directory is litter.

`rationale` is shown in the install preview — "stripped 1 wrapper directory" —
so a user can tell whether Onera understood their archive before approving it.

## 5. `validate_target`

Refuse targets that must never be written. For Cyberpunk:

- `bin/x64/Cyberpunk2077.exe` and `REDprelauncher.exe` — replacing a game
  executable turns a mod install into unrecoverable corruption;
- anything under `archive/pc/content/` — that is the base game's own archives;
  mods belong in `archive/pc/mod/`.

Rejected targets appear in the preview as `InvalidTarget` with your message, so
be specific about _why_ and where the file should have gone.

## 6. Baseline scope

These two have working defaults, so a new adapter can ignore them at first.

`baseline_roots` defaults to the deployment roots of kind `GameInstall` and
`Auxiliary` — the store-managed locations. User-data and compatibility-prefix
roots are dropped, because saves and per-user configuration are not part of what
"clean" means. Override it only if your game keeps store-managed content
somewhere `deploy_roots` does not mention.

`baseline_exclusions` defaults to empty and is worth filling in: anything the
game rewrites by itself — caches, logs, shader caches, configuration written at
runtime — will otherwise be reported as a modified game file after the first
launch. Declare each one with a reason, so the capture summary can explain what
was skipped and why.

The declarations are fingerprinted into every baseline. Narrowing the list later
invalidates existing baselines rather than quietly making "clean" easier to
reach, so prefer a precise `Prefix` over a broad `DirectoryName`.

## 7. Register and test

```rust
pub fn all_adapters() -> Vec<&'static dyn GameAdapter> {
    vec![&Cyberpunk2077, &SkyrimSpecialEdition, &YourGame]
}
```

Table-test `resolve_layout` against real archive shapes: plain, wrapped, deeply
wrapped, documentation-only, unrecognizable, and ambiguous. Both shipped
adapters are usable templates — note that they assert the _ambiguous_ case
produces an error rather than an arbitrary choice.

Which one to copy depends on your game:

- **`cyberpunk2077.rs`** if archives always name their destination directory at
  the top level. Its resolver only strips wrapper directories, so a mapping is
  the identity function on the surviving path.
- **`skyrimse.rs`** if archives are sometimes relative to a subdirectory. Its
  resolver both strips wrappers _and_ adds a `Data/` component, tries both
  readings, and refuses when they land in different places.

Two lessons from writing the second one, both of which apply to any adapter that
adds or rewrites a component:

**Compare results, not readings.** Counting "how many wrapper depths parse" was
wrong: an archive rooted at `Data/` is always _also_ readable as a wrapper around
a `Data`-relative one, and both place every file identically. That is one
reading, not an ambiguity to ask the user about. Deduplicate by the resulting
target set, then refuse only when the readings genuinely differ.

**Canonicalize the case of directories your game owns.** Archives spell it
`Data`, `data` and `DATA`; the game directory has exactly one of those. Deploying
the archive's spelling verbatim builds a second directory beside the real one on
a case-sensitive filesystem, and the mod appears installed while the engine never
loads it.
