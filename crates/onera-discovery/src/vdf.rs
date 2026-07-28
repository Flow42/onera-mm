//! A minimal reader for Valve's KeyValues (VDF) text format.
//!
//! Steam's `libraryfolders.vdf` and `appmanifest_*.acf` are both this format.
//! Onera only needs to read a handful of string values out of them, so this is
//! a deliberately small parser rather than a dependency: it handles nested
//! objects, quoted keys and values, escapes and comments, and nothing else.
//!
//! Parsing these files is what lets Onera avoid scanning the disk. A recursive
//! walk of a user's drives to find games is slow, wakes spinning disks and
//! produces false positives; the library metadata says exactly where each game
//! is.

use std::collections::BTreeMap;

/// A parsed KeyValues node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A leaf string.
    String(String),
    /// A nested object. Keys are compared case-insensitively by
    /// [`Value::get`], as Steam writes them inconsistently.
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// Look up a child by key, case-insensitively.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        let Value::Object(map) = self else {
            return None;
        };
        map.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    /// Read a child as a string.
    #[must_use]
    pub fn string(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::String(s) => Some(s),
            Value::Object(_) => None,
        }
    }

    /// Iterate a child object's entries.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        match self {
            Value::Object(map) => itertools_entries(map),
            Value::String(_) => itertools_entries(EMPTY.get_or_init(BTreeMap::new)),
        }
    }
}

static EMPTY: std::sync::OnceLock<BTreeMap<String, Value>> = std::sync::OnceLock::new();

fn itertools_entries(
    map: &BTreeMap<String, Value>,
) -> std::collections::btree_map::Iter<'_, String, Value> {
    map.iter()
}

/// Why a VDF file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VdfError {
    /// The file ended in the middle of a construct.
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// A brace did not match.
    #[error("unbalanced braces")]
    Unbalanced,
}

/// Parse a KeyValues document into a root object.
///
/// # Errors
/// Fails on truncated input or unbalanced braces. Unknown keys are kept; the
/// caller decides what it cares about.
pub fn parse(input: &str) -> Result<Value, VdfError> {
    let mut chars = input.chars().peekable();
    let map = parse_object(&mut chars, true)?;
    Ok(Value::Object(map))
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

fn parse_object(
    chars: &mut Chars<'_>,
    top_level: bool,
) -> Result<BTreeMap<String, Value>, VdfError> {
    let mut map = BTreeMap::new();
    loop {
        skip_trivia(chars);
        match chars.peek() {
            None => {
                if top_level {
                    return Ok(map);
                }
                return Err(VdfError::UnexpectedEof);
            }
            Some('}') => {
                chars.next();
                if top_level {
                    return Err(VdfError::Unbalanced);
                }
                return Ok(map);
            }
            Some(_) => {}
        }

        let key = parse_token(chars)?;
        skip_trivia(chars);
        match chars.peek() {
            Some('{') => {
                chars.next();
                map.insert(key, Value::Object(parse_object(chars, false)?));
            }
            Some(_) => {
                let value = parse_token(chars)?;
                map.insert(key, Value::String(value));
            }
            None => return Err(VdfError::UnexpectedEof),
        }
    }
}

fn parse_token(chars: &mut Chars<'_>) -> Result<String, VdfError> {
    skip_trivia(chars);
    let mut out = String::new();
    match chars.peek() {
        Some('"') => {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' => return Ok(out),
                    '\\' => match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some(other) => out.push(other),
                        None => return Err(VdfError::UnexpectedEof),
                    },
                    other => out.push(other),
                }
            }
            Err(VdfError::UnexpectedEof)
        }
        Some(_) => {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '{' || c == '}' {
                    break;
                }
                out.push(c);
                chars.next();
            }
            Ok(out)
        }
        None => Err(VdfError::UnexpectedEof),
    }
}

fn skip_trivia(chars: &mut Chars<'_>) {
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        // Line comments start with `//`.
        if chars.peek() == Some(&'/') {
            let mut lookahead = chars.clone();
            lookahead.next();
            if lookahead.peek() == Some(&'/') {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                continue;
            }
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY_FOLDERS: &str = r#"
"libraryfolders"
{
    "0"
    {
        "path"        "/home/user/.local/share/Steam"
        "label"       ""
        "apps"
        {
            "1091500"    "72266008064"
            "570"        "12345"
        }
    }
    // A second library on another drive.
    "1"
    {
        "path"        "/mnt/games/SteamLibrary"
        "apps"
        {
            "1091500"    "72266008064"
        }
    }
}
"#;

    #[test]
    fn parses_library_folders() {
        let root = parse(LIBRARY_FOLDERS).unwrap();
        let folders = root.get("libraryfolders").unwrap();
        let first = folders.get("0").unwrap();
        assert_eq!(first.string("path"), Some("/home/user/.local/share/Steam"));
        assert!(first.get("apps").unwrap().get("1091500").is_some());
        assert_eq!(folders.entries().count(), 2);
    }

    #[test]
    fn keys_are_matched_case_insensitively() {
        // Steam has written both `LibraryFolders` and `libraryfolders`.
        let root = parse(r#""LibraryFolders" { "0" { "Path" "/x" } }"#).unwrap();
        assert_eq!(
            root.get("libraryfolders")
                .unwrap()
                .get("0")
                .unwrap()
                .string("path"),
            Some("/x")
        );
    }

    #[test]
    fn handles_escapes_and_unquoted_tokens() {
        let root = parse(r#""a" "line\nbreak"  b c"#).unwrap();
        assert_eq!(root.string("a"), Some("line\nbreak"));
        assert_eq!(root.string("b"), Some("c"));
    }

    #[test]
    fn comments_are_ignored() {
        let root = parse("// leading comment\n\"a\" \"1\" // trailing\n\"b\" \"2\"").unwrap();
        assert_eq!(root.string("a"), Some("1"));
        assert_eq!(root.string("b"), Some("2"));
    }

    #[test]
    fn rejects_truncated_and_unbalanced_input() {
        assert_eq!(parse(r#""a" { "b" "c""#), Err(VdfError::UnexpectedEof));
        assert_eq!(parse(r#""a" "unterminated"#), Err(VdfError::UnexpectedEof));
        assert_eq!(parse("}"), Err(VdfError::Unbalanced));
    }

    #[test]
    fn an_empty_document_parses_to_an_empty_object() {
        assert_eq!(parse("").unwrap(), Value::Object(BTreeMap::new()));
        assert_eq!(parse("   \n  ").unwrap().entries().count(), 0);
    }

    #[test]
    fn string_lookups_on_a_leaf_return_none() {
        let root = parse(r#""a" "1""#).unwrap();
        assert_eq!(root.get("a").unwrap().string("anything"), None);
        assert_eq!(root.get("a").unwrap().entries().count(), 0);
    }
}
