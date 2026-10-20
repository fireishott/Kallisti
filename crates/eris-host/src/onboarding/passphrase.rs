//! The six-word setup passphrase, generated once on the first boot of a `setup`
//! state database.
//!
//! Three things are deliberately structural rather than documented:
//!
//! - the words come from [`crate::onboarding::wordlist::EFF_SHORT_WORDS`], a
//!   real 1296-word diceware list, so "6 words" is a measurable 62 bits and not
//!   six nouns an author liked;
//! - the value is written to `~/.config/kallisti/first-run.txt` with mode
//!   `0600` and printed once, at generation time, and never again once the
//!   daemon is `ready`;
//! - comparison happens against an Argon2id verifier held in `settings`, not
//!   against the file, so a hand-edited `first-run.txt` cannot widen access.
//!
//! The file exists for the human, not for the verifier.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::wordlist::EFF_SHORT_WORDS;

/// Words in one passphrase.
pub const WORD_COUNT: usize = 6;

/// File name inside `~/.config/kallisti`.
pub const FIRST_RUN_FILE_NAME: &str = "first-run.txt";

/// Mode the file must end up with: owner read and write, nothing else.
pub const FIRST_RUN_MODE: u32 = 0o600;

/// Why a passphrase could not be generated or written.
#[derive(Debug, thiserror::Error)]
pub enum PassphraseError {
    /// The OS random source failed.
    #[error("the OS random source failed")]
    Csprng,
    /// The embedded wordlist is not the documented length, so the entropy claim
    /// would be a lie.
    #[error(
        "the embedded wordlist holds {found} words, but the passphrase is defined over {expected}"
    )]
    WordlistLength {
        /// Words actually present.
        found: usize,
        /// Words the design requires.
        expected: usize,
    },
    /// The file could not be written.
    #[error("cannot write {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The file was written with a wider mode than the design allows.
    #[error("first-run file {path} is mode {actual:04o}, but {expected:04o} is required")]
    InsecureMode {
        /// Path that failed the check.
        path: PathBuf,
        /// Mode found, masked to permission bits.
        actual: u32,
        /// Mode required.
        expected: u32,
    },
}

/// Draw [`WORD_COUNT`] words, joined by single spaces.
///
/// The index comes from the OS CSPRNG with rejection sampling rather than a
/// modulo: 2^32 is not a multiple of 1296, and a plain `%` would bias the first
/// 736 indices by one part in 3.3 million. That is a rounding error, but a
/// passphrase whose entropy argument depends on the draw should not carry one.
pub fn generate() -> Result<String, PassphraseError> {
    let len = EFF_SHORT_WORDS.len();
    if len != 1296 {
        return Err(PassphraseError::WordlistLength {
            found: len,
            expected: 1296,
        });
    }
    let span = u32::try_from(len).map_err(|_| PassphraseError::WordlistLength {
        found: len,
        expected: 1296,
    })?;
    // Largest multiple of `span` that fits in a u32 remainder space.
    let limit = u32::MAX - (u32::MAX % span);

    let mut words: Vec<&'static str> = Vec::with_capacity(WORD_COUNT);
    while words.len() < WORD_COUNT {
        let mut buffer = [0u8; 4];
        getrandom::fill(&mut buffer).map_err(|_| PassphraseError::Csprng)?;
        let draw = u32::from_le_bytes(buffer);
        if draw > limit {
            continue;
        }
        words.push(EFF_SHORT_WORDS[(draw % span) as usize]);
    }
    Ok(words.join(" "))
}

/// Canonical form for comparison: lowercased, trimmed, single-spaced.
///
/// The passphrase is copied by hand, so `"Acorn  acre\nacts"` has to be the same
/// value as `"acorn acre acts"`. Nothing else is forgiven: the words must be the
/// right words in the right order.
#[must_use]
pub fn normalize(input: &str) -> String {
    input
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Write the passphrase to `first-run.txt` at mode `0600`.
///
/// The file is created with the mode in the same call and then re-asserted, so
/// a permissive umask or a pre-existing wider file both end up at `0600`. The
/// value is not logged: the only output is the file, and the caller prints the
/// human block to stdout itself.
pub fn write_first_run(path: &Path, passphrase: &str) -> Result<(), PassphraseError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| PassphraseError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FIRST_RUN_MODE)
        .open(path)
        .map_err(|source| PassphraseError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let body = format!(
        "# Kallisti 1.0 setup passphrase. Six words, generated once at first boot.\n\
         # Enter it at the onboarding wizard. This file is mode 0600.\n\
         # After onboarding this passphrase is the admin passphrase that\n\
         # `eris setup --reset` asks for.\n\
         {passphrase}\n"
    );
    file.write_all(body.as_bytes())
        .map_err(|source| PassphraseError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.sync_all().map_err(|source| PassphraseError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    drop(file);

    let mut permissions = fs::metadata(path)
        .map_err(|source| PassphraseError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, FIRST_RUN_MODE);
    fs::set_permissions(path, permissions).map_err(|source| PassphraseError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let actual = fs::metadata(path)
        .map(|metadata| std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o777)
        .map_err(|source| PassphraseError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if actual != FIRST_RUN_MODE {
        return Err(PassphraseError::InsecureMode {
            path: path.to_path_buf(),
            actual,
            expected: FIRST_RUN_MODE,
        });
    }
    Ok(())
}

/// The one block printed at first boot.
///
/// Plain ASCII, no colour, no em dashes. The SSH tunnel command is the only
/// supported way in from another machine: the bind is loopback and stays
/// loopback, so a Serve mapping on top of it is not part of 1.0.
#[must_use]
pub fn first_run_block(
    passphrase: &str,
    first_run_path: &Path,
    user: &str,
    host: &str,
    port: u16,
) -> String {
    format!(
        "\n\
         Kallisti 1.0 - first run\n\
         ========================\n\
         \n\
         Setup passphrase: {passphrase}\n\
         \n\
         Written to {file} (mode 0600). It is not printed again once\n\
         onboarding completes.\n\
         \n\
         Reach the wizard from another machine over SSH, never Tailscale:\n\
         \n\
           ssh -L {port}:127.0.0.1:{port} {user}@{host}\n\
           then open http://127.0.0.1:{port}/setup\n\
         \n\
           http://127.0.0.1:{port}/setup\n\
         \n\
         On this host the wizard is already at the loopback address above.\n\
         Waiting for onboarding.\n",
        file = first_run_path.display()
    )
}

/// The line printed on a later boot while onboarding is still pending: where the
/// passphrase is, never the passphrase itself.
#[must_use]
pub fn pending_block(first_run_path: &Path, port: u16) -> String {
    format!(
        "\n\
         Kallisti 1.0 - onboarding pending\n\
         ================================\n\
         \n\
         The setup passphrase was generated on the first boot and is in\n\
         {file} (mode 0600).\n\
         \n\
           http://127.0.0.1:{port}/setup\n\
         \n\
         Waiting for onboarding.\n",
        file = first_run_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wordlist_is_the_documented_length_with_no_repeats() {
        assert_eq!(EFF_SHORT_WORDS.len(), 1296);
        let mut sorted = EFF_SHORT_WORDS;
        sorted.sort_unstable();
        let unique = sorted.windows(2).filter(|pair| pair[0] != pair[1]).count() + 1;
        assert_eq!(unique, 1296, "a duplicate word would change the entropy");
        for word in EFF_SHORT_WORDS {
            assert!(
                word.chars()
                    .all(|character| character.is_ascii_lowercase() || character == '-'),
                "{word} is not a lowercase ASCII word"
            );
            assert!((3..=5).contains(&word.len()), "{word} is {word} characters");
        }
    }

    #[test]
    fn a_passphrase_is_six_distinct_list_words() {
        let passphrase = generate().expect("the OS random source works");
        let words: Vec<&str> = passphrase.split(' ').collect();
        assert_eq!(words.len(), WORD_COUNT);
        for word in &words {
            assert!(
                EFF_SHORT_WORDS.contains(word),
                "{word} is not from the embedded list"
            );
        }
        let mut unique = words.clone();
        unique.sort_unstable();
        unique.dedup();
        assert!(
            unique.len() > 1,
            "six identical draws would mean the source is not random: {passphrase}"
        );
    }

    #[test]
    fn two_generated_passphrases_differ() {
        let first = generate().expect("draw");
        let second = generate().expect("draw");
        assert_ne!(first, second, "1296^6 draws colliding is not a coincidence");
    }

    #[test]
    fn normalization_folds_case_and_whitespace_only() {
        assert_eq!(normalize("  Acorn   ACRE\nacts "), "acorn acre acts");
        assert_eq!(normalize("acorn acre acts"), "acorn acre acts");
        assert_ne!(normalize("acorn acre acts"), normalize("acts acre acorn"));
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn the_first_run_file_is_mode_0600_and_holds_the_passphrase() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("first-run.txt");
        write_first_run(&path, "acorn acre acts afar acid also").expect("write");

        let mode = std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(&path).expect("metadata").permissions(),
        ) & 0o777;
        assert_eq!(mode, FIRST_RUN_MODE, "the file must not be group readable");

        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("acorn acre acts afar acid also"));
        assert!(
            text.contains("eris setup --reset"),
            "the recovery path is named"
        );
    }

    #[test]
    fn the_printed_block_names_the_tunnel_and_the_url() {
        let path = PathBuf::from("/home/example/.config/kallisti/first-run.txt");
        let block = first_run_block(
            "acid acorn acre acts afar also",
            &path,
            "operator",
            "host",
            7340,
        );
        assert!(block.contains("ssh -L 7340:127.0.0.1:7340 operator@host"));
        assert!(block.contains("http://127.0.0.1:7340/setup"));
        assert!(block.contains("acid acorn acre acts afar also"));
        assert!(block.contains(&path.display().to_string()));
        assert!(!block.contains("Tailscale serve"));
    }

    #[test]
    fn the_pending_block_holds_no_passphrase() {
        let path = PathBuf::from("/home/example/.config/kallisti/first-run.txt");
        let block = pending_block(&path, 7340);
        assert!(block.contains("first-run.txt"));
        assert!(block.contains("http://127.0.0.1:7340/setup"));
        assert!(
            !block.contains("acorn"),
            "a later boot must not print a word that could be the passphrase"
        );
    }
}
