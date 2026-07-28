//! Normalized relative paths.
//!
//! [`RelPath`] is the only path type allowed to cross the boundary between an
//! archive and the filesystem. Construction is fallible and rejects everything
//! that could escape a root:
//!
//! * absolute paths (`/etc/passwd`)
//! * Windows drive prefixes (`C:\...`) and UNC prefixes (`\\host\share`)
//! * any `..` component, before or after normalization
//! * `.` components and empty segments (collapsed, or rejected if nothing remains)
//! * NUL bytes and control characters
//! * over-long components or total lengths
//!
//! Backslashes are treated as separators. Archives produced on Windows
//! routinely use `\`, and a `..\..\x` entry must not survive normalization just
//! because Linux considers `\` an ordinary filename character. The cost is that
//! a genuine Linux filename containing a backslash is split; that is deliberate
//! and documented in `docs/threat-model.md`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum number of bytes in a single path component.
pub const MAX_COMPONENT_LEN: usize = 255;
/// Maximum number of bytes in a whole relative path.
pub const MAX_PATH_LEN: usize = 4096;
/// Maximum number of components in a relative path.
pub const MAX_DEPTH: usize = 64;

/// Why a path could not be normalized into a [`RelPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelPathError {
    /// The path was empty, or normalized away to nothing.
    #[error("path is empty after normalization")]
    Empty,
    /// The path was rooted at the filesystem root.
    #[error("absolute paths are not allowed: {0:?}")]
    Absolute(String),
    /// The path carried a `C:` style or UNC prefix.
    #[error("drive or UNC prefixed paths are not allowed: {0:?}")]
    DrivePrefix(String),
    /// A `..` component was present.
    #[error("path escapes its root: {0:?}")]
    Traversal(String),
    /// A NUL or other control character was present.
    #[error("path contains a control character: {0:?}")]
    ControlCharacter(String),
    /// A single component exceeded [`MAX_COMPONENT_LEN`].
    #[error("path component exceeds {MAX_COMPONENT_LEN} bytes: {0:?}")]
    ComponentTooLong(String),
    /// The whole path exceeded [`MAX_PATH_LEN`].
    #[error("path exceeds {MAX_PATH_LEN} bytes")]
    TooLong,
    /// The path had more than [`MAX_DEPTH`] components.
    #[error("path exceeds {MAX_DEPTH} components")]
    TooDeep,
}

/// A validated, normalized, `/`-separated relative path.
///
/// The invariant is total: for any `RelPath` `p` and any directory `root`,
/// `root.join(p.as_str())` is lexically inside `root`. This is verified by a
/// property test in `tests/` and by `proptest` in this module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    /// Normalize and validate an arbitrary, untrusted path string.
    ///
    /// # Errors
    /// Returns a [`RelPathError`] if the input cannot be represented as a safe
    /// relative path.
    pub fn normalize(input: &str) -> Result<Self, RelPathError> {
        if input.is_empty() {
            return Err(RelPathError::Empty);
        }
        if input.len() > MAX_PATH_LEN {
            return Err(RelPathError::TooLong);
        }
        if input.chars().any(|c| c.is_control()) {
            return Err(RelPathError::ControlCharacter(input.to_owned()));
        }
        // Order matters: a leading `/` is reported as absolute even when it is
        // doubled, so POSIX-rooted paths never get filed under a Windows-shaped
        // error.
        if input.starts_with('/') {
            return Err(RelPathError::Absolute(input.to_owned()));
        }
        if has_drive_or_unc_prefix(input) {
            return Err(RelPathError::DrivePrefix(input.to_owned()));
        }
        if input.starts_with('\\') {
            return Err(RelPathError::Absolute(input.to_owned()));
        }

        let mut parts: Vec<&str> = Vec::new();
        for raw in input.split(['/', '\\']) {
            match raw {
                "" | "." => continue,
                ".." => return Err(RelPathError::Traversal(input.to_owned())),
                segment => {
                    // A trailing dot or space is stripped by some filesystems,
                    // which would silently alias two different entries.
                    let trimmed = segment.trim_end_matches([' ', '.']);
                    if trimmed.is_empty() {
                        // A component of only dots/spaces, e.g. "..." or " ".
                        return Err(RelPathError::Traversal(input.to_owned()));
                    }
                    if segment.len() > MAX_COMPONENT_LEN {
                        return Err(RelPathError::ComponentTooLong(segment.to_owned()));
                    }
                    parts.push(segment);
                }
            }
        }

        if parts.is_empty() {
            return Err(RelPathError::Empty);
        }
        if parts.len() > MAX_DEPTH {
            return Err(RelPathError::TooDeep);
        }
        let joined = parts.join("/");
        if joined.len() > MAX_PATH_LEN {
            return Err(RelPathError::TooLong);
        }
        Ok(Self(joined))
    }

    /// Borrow the normalized path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The components of the path, never empty.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// Number of components.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.components().count()
    }

    /// The final component.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// The lowercase extension without the dot, if any.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        let name = self.file_name();
        let idx = name.rfind('.')?;
        if idx == 0 || idx + 1 == name.len() {
            return None;
        }
        Some(name[idx + 1..].to_ascii_lowercase())
    }

    /// The first component, useful for top-level layout detection.
    #[must_use]
    pub fn first_component(&self) -> &str {
        self.components().next().unwrap_or_default()
    }

    /// The path without its final component, or `None` for a single-component
    /// path.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let idx = self.0.rfind('/')?;
        Some(Self(self.0[..idx].to_owned()))
    }

    /// Drop the first `n` components. Returns `None` if nothing would remain.
    #[must_use]
    pub fn strip_prefix_components(&self, n: usize) -> Option<Self> {
        let rest: Vec<&str> = self.components().skip(n).collect();
        if rest.is_empty() {
            return None;
        }
        Some(Self(rest.join("/")))
    }

    /// Prefix this path with another relative path.
    ///
    /// # Errors
    /// Fails only if the concatenation exceeds the length or depth limits.
    pub fn prefixed_with(&self, prefix: &RelPath) -> Result<Self, RelPathError> {
        Self::normalize(&format!("{}/{}", prefix.0, self.0))
    }

    /// Resolve against a filesystem root.
    ///
    /// Purely lexical: because of the `RelPath` invariant the result is always
    /// inside `root`. Symlinked *ancestors* are the caller's problem and are
    /// handled by the installer, which opens roots with `O_DIRECTORY` semantics
    /// and re-verifies after writing.
    #[must_use]
    pub fn resolve_under(&self, root: &std::path::Path) -> std::path::PathBuf {
        let mut out = root.to_path_buf();
        for component in self.components() {
            out.push(component);
        }
        out
    }

    /// Case-insensitive comparison key, used to detect targets that would
    /// collide on a case-insensitive filesystem or inside a Proton prefix.
    #[must_use]
    pub fn case_fold_key(&self) -> String {
        self.0.to_lowercase()
    }
}

fn has_drive_or_unc_prefix(input: &str) -> bool {
    let bytes = input.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return true;
    }
    input.starts_with("\\\\")
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for RelPath {
    type Error = RelPathError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::normalize(&value)
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.0
    }
}

/// The kind of root a file is deployed into.
///
/// Game adapters describe *where* things go without the installer knowing
/// anything about the game. The concrete directory for each kind comes from
/// [`crate::domain::game::DeployRoot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployRootKind {
    /// The game installation directory itself.
    GameInstall,
    /// A user-data directory (saves, per-user config), outside the install.
    UserData,
    /// A compatibility prefix (Proton/Wine) drive root.
    CompatPrefix,
    /// A game-specific auxiliary root declared by the adapter.
    Auxiliary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::{Component, Path, PathBuf};

    #[test]
    fn accepts_plain_paths() {
        let p = RelPath::normalize("archive/pc/mod/foo.archive").unwrap();
        assert_eq!(p.as_str(), "archive/pc/mod/foo.archive");
        assert_eq!(p.file_name(), "foo.archive");
        assert_eq!(p.extension().as_deref(), Some("archive"));
        assert_eq!(p.first_component(), "archive");
        assert_eq!(p.depth(), 4);
    }

    #[test]
    fn collapses_redundant_segments() {
        assert_eq!(RelPath::normalize("./a//b/./c").unwrap().as_str(), "a/b/c");
    }

    #[test]
    fn converts_backslash_separators() {
        assert_eq!(RelPath::normalize(r"a\b\c").unwrap().as_str(), "a/b/c");
    }

    #[test]
    fn rejects_traversal() {
        for bad in [
            "../etc/passwd",
            "a/../../b",
            r"..\..\windows\system32",
            "a/..",
            "...",
            "a/.../b",
        ] {
            assert!(
                matches!(RelPath::normalize(bad), Err(RelPathError::Traversal(_))),
                "expected traversal rejection for {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_absolute_and_drive() {
        assert!(matches!(
            RelPath::normalize("/etc/passwd"),
            Err(RelPathError::Absolute(_))
        ));
        assert!(matches!(
            RelPath::normalize(r"C:\Windows\system32"),
            Err(RelPathError::DrivePrefix(_))
        ));
        assert!(matches!(
            RelPath::normalize(r"\\server\share\x"),
            Err(RelPathError::DrivePrefix(_))
        ));
        // A POSIX-rooted path is absolute, however many slashes it starts with.
        for absolute in ["//server/share/x", "///", "//"] {
            assert!(matches!(
                RelPath::normalize(absolute),
                Err(RelPathError::Absolute(_))
            ));
        }
    }

    #[test]
    fn rejects_control_characters() {
        assert!(matches!(
            RelPath::normalize("a/b\0c"),
            Err(RelPathError::ControlCharacter(_))
        ));
        assert!(matches!(
            RelPath::normalize("a/b\nc"),
            Err(RelPathError::ControlCharacter(_))
        ));
    }

    #[test]
    fn rejects_limits() {
        let long_component = "x".repeat(MAX_COMPONENT_LEN + 1);
        assert!(matches!(
            RelPath::normalize(&long_component),
            Err(RelPathError::ComponentTooLong(_))
        ));
        let deep = vec!["a"; MAX_DEPTH + 1].join("/");
        assert!(matches!(
            RelPath::normalize(&deep),
            Err(RelPathError::TooDeep)
        ));
        let long = format!("a/{}", "b".repeat(MAX_PATH_LEN));
        assert!(matches!(
            RelPath::normalize(&long),
            Err(RelPathError::TooLong)
        ));
    }

    #[test]
    fn empty_inputs_are_rejected() {
        for bad in ["", ".", "./", "./././", ".//./"] {
            assert!(
                matches!(RelPath::normalize(bad), Err(RelPathError::Empty)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn parent_drops_the_last_component() {
        let p = RelPath::normalize("a/b/c.txt").unwrap();
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(p.parent().unwrap().parent().unwrap().as_str(), "a");
        assert_eq!(RelPath::normalize("top").unwrap().parent(), None);
    }

    #[test]
    fn strips_prefix_components() {
        let p = RelPath::normalize("wrapper/archive/pc/mod/x.archive").unwrap();
        assert_eq!(
            p.strip_prefix_components(1).unwrap().as_str(),
            "archive/pc/mod/x.archive"
        );
        assert!(RelPath::normalize("only")
            .unwrap()
            .strip_prefix_components(1)
            .is_none());
    }

    /// Lexical containment check that does not touch the filesystem.
    fn is_lexically_inside(root: &Path, candidate: &Path) -> bool {
        let mut stack: Vec<&std::ffi::OsStr> = Vec::new();
        let Ok(rest) = candidate.strip_prefix(root) else {
            return false;
        };
        for component in rest.components() {
            match component {
                Component::Normal(c) => stack.push(c),
                Component::CurDir => {}
                // Any of these mean we left the root or were never relative.
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            }
        }
        true
    }

    proptest! {
        /// The central security property: whatever bytes an archive supplies,
        /// a successfully normalized path always resolves inside its root.
        #[test]
        fn normalized_paths_never_escape_root(raw in ".{0,200}") {
            if let Ok(rel) = RelPath::normalize(&raw) {
                let root = PathBuf::from("/staging/op-1");
                let resolved = rel.resolve_under(&root);
                prop_assert!(
                    is_lexically_inside(&root, &resolved),
                    "escaped: {raw:?} -> {resolved:?}"
                );
                prop_assert!(!rel.as_str().is_empty());
                prop_assert!(!rel.components().any(|c| c.is_empty() || c == "." || c == ".."));
            }
        }

        /// Normalization is idempotent: re-normalizing a `RelPath` is a no-op.
        #[test]
        fn normalization_is_idempotent(raw in ".{0,200}") {
            if let Ok(once) = RelPath::normalize(&raw) {
                let twice = RelPath::normalize(once.as_str()).expect("already normalized");
                prop_assert_eq!(once.as_str(), twice.as_str());
            }
        }

        /// Traversal cannot be smuggled in by mixing separators or padding.
        #[test]
        fn traversal_is_always_rejected(
            prefix in "[a-z/]{0,20}",
            sep in prop::sample::select(vec!["/", "\\"]),
            suffix in "[a-z/]{0,20}",
        ) {
            let raw = format!("{prefix}{sep}..{sep}{suffix}");
            if let Ok(rel) = RelPath::normalize(&raw) {
                prop_assert!(false, "accepted traversal {raw:?} as {rel}");
            }
        }
    }
}
