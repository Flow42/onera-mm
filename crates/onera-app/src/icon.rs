//! Finding a game's own icon in its installation directory.
//!
//! A registered installation is a directory and an adapter id; Onera has no
//! artwork for it and no catalogue to ask. Most games do ship a picture of
//! themselves though — an installer icon, a launcher logo — so the directory is
//! searched for one rather than a placeholder being invented.
//!
//! Two rules keep this from being a liability:
//!
//! * the search is **bounded** — a shallow walk, a capped number of entries and
//!   a capped file size — so pointing Onera at a directory with a million files
//!   costs a fixed amount of work rather than a hang;
//! * nothing is decoded. The bytes are handed to the window with the type the
//!   extension implies, and the browser engine decides whether they are an
//!   image. Onera never parses an untrusted image format itself.
//!
//! Finding nothing is the ordinary case for plenty of games, and is not an
//! error: the card falls back to the initials it already draws.

use crate::flow::Onera;
use onera_core::ids::LocalGameId;
use onera_core::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Directory levels below the installation root that are searched.
///
/// Icons live at the top or one level down (`bin/`, `Data/`, `support/`).
/// Deeper than that is a texture pack, not a game's identity.
const MAX_DEPTH: usize = 2;

/// Directory entries examined before the search gives up.
const MAX_ENTRIES: usize = 4096;

/// Largest icon Onera will read and hand to the window.
///
/// An icon is tens of kilobytes; the bound is what stops a 900 MB file that
/// happens to end in `.png` from being loaded into memory and then into a data
/// URI, which would cost several times its size again.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// An image found next to a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameIcon {
    /// Encoded bytes, exactly as they are on disk.
    pub bytes: Vec<u8>,
    /// MIME type implied by the file's extension.
    pub content_type: String,
    /// Where it was found, so the choice can be explained.
    pub source: PathBuf,
}

impl Onera {
    /// The best icon in a game's installation directory, if it has one.
    ///
    /// # Errors
    /// Propagates database errors while resolving the game. A directory that
    /// cannot be read is not an error — it is a game with no icon.
    pub async fn game_icon(&self, game: LocalGameId) -> Result<Option<GameIcon>> {
        let install = self
            .database()
            .local_installs()
            .await?
            .into_iter()
            .find(|candidate| candidate.id == game)
            .ok_or_else(|| CoreError::NotFound {
                kind: "game installation",
                id: game.to_string(),
            })?;
        Ok(find_icon(&install.install_root).await)
    }
}

/// Search one directory tree for the most icon-like image in it.
async fn find_icon(root: &Path) -> Option<GameIcon> {
    let name_hint = root
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut best: Option<(u32, PathBuf)> = None;
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0_usize)]);
    let mut seen = 0_usize;

    while let Some((directory, depth)) = queue.pop_front() {
        let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            seen += 1;
            if seen > MAX_ENTRIES {
                queue.clear();
                break;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            // Symlinks are skipped rather than followed: a link out of the
            // installation is a way to make Onera read a file somewhere else.
            if !file_type.is_file() {
                continue;
            }
            let Some(score) = score_candidate(&path, depth, &name_hint) else {
                continue;
            };
            if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
                best = Some((score, path));
            }
        }
    }

    let (_, path) = best?;
    read_icon(&path).await
}

/// How icon-like one file is, or `None` when it is not an image at all.
///
/// The ordering is the interesting part: a file *called* an icon beats a
/// screenshot that happens to sit beside it, a file named after the game beats
/// an unnamed one, and anything at the top of the installation beats the same
/// name buried a level down. Formats are ranked too — an `.ico` in a game
/// directory is nearly always the game's own icon, while a `.jpg` is nearly
/// always a photograph of something.
fn score_candidate(path: &Path, depth: usize, name_hint: &str) -> Option<u32> {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())?;
    let format = match extension.as_str() {
        "ico" => 5,
        "png" => 4,
        "webp" | "gif" => 2,
        "jpg" | "jpeg" | "bmp" => 1,
        _ => return None,
    };
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    let mut score = format;
    if stem.contains("icon") {
        score += 12;
    } else if stem.contains("logo") {
        score += 8;
    }
    // The directory a game is installed in is named after the game far more
    // reliably than any file inside it, so it is the hint used for matching.
    if !name_hint.is_empty() && (stem.contains(name_hint) || name_hint.contains(&stem)) {
        score += 10;
    }
    if depth == 0 {
        score += 3;
    }
    Some(score)
}

/// Read a candidate, refusing anything too large to be an icon.
async fn read_icon(path: &Path) -> Option<GameIcon> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    if metadata.len() > MAX_BYTES || metadata.len() == 0 {
        return None;
    }
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(GameIcon {
        content_type: content_type_of(path).to_owned(),
        bytes,
        source: path.to_path_buf(),
    })
}

/// The MIME type an extension implies.
fn content_type_of(path: &Path) -> &'static str {
    match path
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .as_deref()
    {
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(name: &str, depth: usize, hint: &str) -> Option<u32> {
        score_candidate(Path::new(name), depth, hint)
    }

    #[test]
    fn only_images_are_candidates() {
        assert!(score("Cyberpunk2077.exe", 0, "cyberpunk 2077").is_none());
        assert!(score("readme.txt", 0, "").is_none());
        assert!(score("icon.png", 0, "").is_some());
    }

    #[test]
    fn a_file_that_says_it_is_an_icon_wins() {
        let icon = score("icon.png", 1, "cyberpunk 2077").unwrap();
        let screenshot = score("screenshot.png", 1, "cyberpunk 2077").unwrap();
        assert!(icon > screenshot);
    }

    #[test]
    fn a_file_named_after_the_game_beats_an_anonymous_one() {
        let named = score("cyberpunk 2077.png", 0, "cyberpunk 2077").unwrap();
        let anonymous = score("background.png", 0, "cyberpunk 2077").unwrap();
        assert!(named > anonymous);
    }

    #[test]
    fn the_top_of_the_installation_wins_over_a_copy_further_down() {
        assert!(score("icon.png", 0, "").unwrap() > score("icon.png", 1, "").unwrap());
    }

    #[tokio::test]
    async fn a_game_with_no_image_simply_has_no_icon() {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::write(directory.path().join("game.exe"), b"not an image")
            .await
            .unwrap();
        assert!(find_icon(directory.path()).await.is_none());
    }

    #[tokio::test]
    async fn the_best_candidate_is_read_with_the_type_its_name_implies() {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::write(directory.path().join("screenshot.jpg"), b"jpeg")
            .await
            .unwrap();
        tokio::fs::create_dir(directory.path().join("bin"))
            .await
            .unwrap();
        tokio::fs::write(directory.path().join("bin/icon.ico"), b"icon bytes")
            .await
            .unwrap();

        let found = find_icon(directory.path()).await.unwrap();
        assert_eq!(found.bytes, b"icon bytes");
        assert_eq!(found.content_type, "image/x-icon");
    }

    #[tokio::test]
    async fn an_enormous_file_is_not_loaded_however_it_is_named() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("icon.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        assert!(find_icon(directory.path()).await.is_none());
    }
}
