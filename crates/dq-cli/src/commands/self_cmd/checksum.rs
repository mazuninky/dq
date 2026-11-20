//! SHA-256 verification of a downloaded release archive against the
//! release's `dq-checksums.txt` — the gate `dq self update` runs before it
//! extracts anything.
//!
//! Pure: no network, no filesystem. [`verify_archive_checksum`] takes the
//! archive's file name and bytes plus the text of the checksums asset, so
//! every refusal is testable without a release.
//!
//! # Why `dq-checksums.txt` and not the per-archive `.sha256` sidecar
//!
//! The release workflow publishes both. `dq-checksums.txt` is the one to
//! trust: the release job computes it over the exact archives it uploads
//! (the sidecars are computed in the build job, before the artifact
//! round-trip), it is the file `scripts/install.sh` verifies against, and it
//! is the file the optional cosign step signs — so a future signature check
//! covers the same digest this gate reads.
//!
//! # Format
//!
//! The file is machine-written by `sha256sum` in the release job, so the
//! parser is strict: every non-blank line must be `<64 hex digits>`, one
//! space, a mode marker (` ` text or `*` binary), then the file name. Hex
//! case is normalised. The archive's entry is matched on the exact file name;
//! an entry for a different name does not count.

use thiserror::Error;

/// Name of the release asset `dq self update` reads the expected digest from.
pub const CHECKSUMS_ASSET: &str = "dq-checksums.txt";

/// Length of a SHA-256 digest in hex digits.
const SHA256_HEX_LEN: usize = 64;

/// Why a downloaded archive was refused. Every variant leaves the installed
/// binary untouched; `exit_code_for_error` maps them all to `VALIDATE_FAIL`
/// (4).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChecksumError {
    /// The release carries no `dq-checksums.txt` asset.
    #[error(
        "release has no `{CHECKSUMS_ASSET}` asset; refusing to install an archive \
         whose SHA-256 cannot be verified"
    )]
    MissingAsset,
    /// `dq-checksums.txt` is empty, not UTF-8, or holds a line that is not a
    /// `sha256sum` entry.
    #[error("`{CHECKSUMS_ASSET}` is malformed: {detail}; refusing to install")]
    Malformed {
        /// What is wrong, naming the 1-based line where there is one.
        detail: String,
    },
    /// `dq-checksums.txt` has no entry for the downloaded archive's name.
    #[error("`{CHECKSUMS_ASSET}` has no entry for `{archive}`; refusing to install")]
    NoEntry {
        /// File name of the downloaded archive.
        archive: String,
    },
    /// `dq-checksums.txt` lists the archive more than once with different
    /// digests.
    #[error(
        "`{CHECKSUMS_ASSET}` lists `{archive}` more than once with different digests; \
         refusing to install"
    )]
    Conflicting {
        /// File name of the downloaded archive.
        archive: String,
    },
    /// The archive's SHA-256 differs from the one `dq-checksums.txt` lists.
    #[error(
        "SHA-256 mismatch for `{archive}`: `{CHECKSUMS_ASSET}` lists {expected}, \
         the download hashes to {actual}; refusing to install"
    )]
    Mismatch {
        /// File name of the downloaded archive.
        archive: String,
        /// Digest listed in `dq-checksums.txt`, lowercase hex.
        expected: String,
        /// Digest of the downloaded bytes, lowercase hex.
        actual: String,
    },
}

/// Verify `archive` (the downloaded bytes of the release asset named
/// `archive_name`) against `checksums`, the downloaded bytes of the
/// release's `dq-checksums.txt` (`None` when the release has no such asset).
///
/// Returns the verified digest, lowercase hex.
///
/// # Errors
///
/// [`ChecksumError::MissingAsset`] when `checksums` is `None`;
/// [`ChecksumError::Malformed`] (incl. a file that is not UTF-8),
/// [`ChecksumError::NoEntry`] or [`ChecksumError::Conflicting`] when the
/// file names no single digest for `archive_name`;
/// [`ChecksumError::Mismatch`] when the bytes hash to a different digest.
pub fn verify_archive_checksum(
    archive_name: &str,
    archive: &[u8],
    checksums: Option<&[u8]>,
) -> Result<String, ChecksumError> {
    let checksums = checksums.ok_or(ChecksumError::MissingAsset)?;
    let checksums = std::str::from_utf8(checksums).map_err(|_| ChecksumError::Malformed {
        detail: "the file is not UTF-8".to_owned(),
    })?;
    let expected = expected_digest(checksums, archive_name)?;
    let actual = sha256_hex(archive);
    if actual == expected {
        Ok(actual)
    } else {
        Err(ChecksumError::Mismatch {
            archive: archive_name.to_owned(),
            expected,
            actual,
        })
    }
}

/// The lowercase hex digest `checksums` lists for `archive_name`.
///
/// # Errors
///
/// [`ChecksumError::Malformed`] for an empty file or any line that is not a
/// `sha256sum` entry, [`ChecksumError::NoEntry`] when no line names
/// `archive_name`, [`ChecksumError::Conflicting`] when two lines name it with
/// different digests.
pub fn expected_digest(checksums: &str, archive_name: &str) -> Result<String, ChecksumError> {
    if checksums.trim().is_empty() {
        return Err(ChecksumError::Malformed {
            detail: "the file is empty".to_owned(),
        });
    }
    let mut found: Option<String> = None;
    for (index, line) in checksums.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (digest, name) = parse_line(line).ok_or_else(|| ChecksumError::Malformed {
            detail: format!(
                "line {} is not `<{SHA256_HEX_LEN} hex digits>  <file name>`",
                index + 1
            ),
        })?;
        if name != archive_name {
            continue;
        }
        match &found {
            Some(previous) if *previous != digest => {
                return Err(ChecksumError::Conflicting {
                    archive: archive_name.to_owned(),
                });
            }
            Some(_) => {}
            None => found = Some(digest),
        }
    }
    found.ok_or_else(|| ChecksumError::NoEntry {
        archive: archive_name.to_owned(),
    })
}

/// Split one `sha256sum` line into `(lowercase digest, file name)`.
fn parse_line(line: &str) -> Option<(String, &str)> {
    let digest = line.get(..SHA256_HEX_LEN)?;
    if !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rest = line.get(SHA256_HEX_LEN..)?;
    let name = rest
        .strip_prefix("  ")
        .or_else(|| rest.strip_prefix(" *"))?;
    if name.is_empty() {
        return None;
    }
    Some((digest.to_ascii_lowercase(), name))
}

/// SHA-256 of `bytes`, lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest
        .as_ref()
        .iter()
        .fold(String::with_capacity(SHA256_HEX_LEN), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const ARCHIVE: &str = "dq-2026.21.0-aarch64-apple-darwin.tar.gz";
    /// SHA-256 of `b"hello"`.
    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn sha256_hex_of_known_input() {
        assert_eq!(sha256_hex(b"hello"), HELLO_SHA256);
    }

    #[test]
    fn matching_digest_is_accepted() {
        let sums = format!("{HELLO_SHA256}  {ARCHIVE}\n");
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Ok(HELLO_SHA256.to_owned())
        );
    }

    #[test]
    fn multi_line_file_picks_the_archive_entry() {
        let sums = format!(
            "{zero}  dq-2026.21.0-x86_64-unknown-linux-gnu.tar.gz\n\
             {HELLO_SHA256}  {ARCHIVE}\n\
             {zero}  dq-2026.21.0-aarch64-unknown-linux-gnu.tar.gz\n",
            zero = "0".repeat(64),
        );
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Ok(HELLO_SHA256.to_owned())
        );
    }

    #[test]
    fn uppercase_hex_and_binary_marker_are_accepted() {
        let sums = format!("{} *{ARCHIVE}\n", HELLO_SHA256.to_ascii_uppercase());
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Ok(HELLO_SHA256.to_owned())
        );
    }

    #[test]
    fn mismatch_is_refused() {
        let sums = format!("{HELLO_SHA256}  {ARCHIVE}\n");
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"tampered", Some(sums.as_bytes())),
            Err(ChecksumError::Mismatch {
                archive: ARCHIVE.to_owned(),
                expected: HELLO_SHA256.to_owned(),
                actual: sha256_hex(b"tampered"),
            })
        );
    }

    #[test]
    fn missing_asset_is_refused() {
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", None),
            Err(ChecksumError::MissingAsset)
        );
    }

    #[test]
    fn entry_for_a_different_name_is_refused() {
        // The sidecar of the archive, or another target's archive, carries a
        // valid digest — but not for the file that was downloaded.
        let sums = format!(
            "{HELLO_SHA256}  {ARCHIVE}.sha256\n{HELLO_SHA256}  dq-2026.21.0-x86_64-unknown-linux-gnu.tar.gz\n"
        );
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Err(ChecksumError::NoEntry {
                archive: ARCHIVE.to_owned()
            })
        );
    }

    #[test]
    fn name_with_a_path_prefix_does_not_count() {
        let sums = format!("{HELLO_SHA256}  dist/{ARCHIVE}\n");
        assert!(matches!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Err(ChecksumError::NoEntry { .. })
        ));
    }

    #[test]
    fn malformed_files_are_refused() {
        for sums in [
            String::new(),
            "\n  \n".to_owned(),
            // A bare digest names no file.
            format!("{HELLO_SHA256}\n"),
            // Short digest.
            format!("{}  {ARCHIVE}\n", &HELLO_SHA256[..63]),
            // Non-hex digit.
            format!("g{}  {ARCHIVE}\n", &HELLO_SHA256[1..]),
            // One space and no mode marker.
            format!("{HELLO_SHA256} {ARCHIVE}\n"),
            // BSD tag form is not what the release job writes.
            format!("SHA256 ({ARCHIVE}) = {HELLO_SHA256}\n"),
            // A good entry does not excuse a bad line elsewhere.
            format!("{HELLO_SHA256}  {ARCHIVE}\n# comment\n"),
        ] {
            assert!(
                matches!(
                    verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
                    Err(ChecksumError::Malformed { .. })
                ),
                "must be refused as malformed: {sums:?}"
            );
        }
    }

    #[test]
    fn malformed_names_the_line() {
        let sums = format!("{HELLO_SHA256}  {ARCHIVE}\nnot a checksum line\n");
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Err(ChecksumError::Malformed {
                detail: "line 2 is not `<64 hex digits>  <file name>`".to_owned()
            })
        );
    }

    #[test]
    fn conflicting_entries_are_refused() {
        let sums = format!("{HELLO_SHA256}  {ARCHIVE}\n{}  {ARCHIVE}\n", "0".repeat(64));
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Err(ChecksumError::Conflicting {
                archive: ARCHIVE.to_owned()
            })
        );
    }

    #[test]
    fn duplicate_identical_entries_are_accepted() {
        let sums = format!(
            "{HELLO_SHA256}  {ARCHIVE}\n{}  {ARCHIVE}\n",
            HELLO_SHA256.to_ascii_uppercase()
        );
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Ok(HELLO_SHA256.to_owned())
        );
    }

    #[test]
    fn non_utf8_file_is_refused() {
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(b"\xff\xfe")),
            Err(ChecksumError::Malformed {
                detail: "the file is not UTF-8".to_owned()
            })
        );
    }

    #[test]
    fn crlf_line_ends_are_accepted() {
        let sums = format!("{HELLO_SHA256}  {ARCHIVE}\r\n");
        assert_eq!(
            verify_archive_checksum(ARCHIVE, b"hello", Some(sums.as_bytes())),
            Ok(HELLO_SHA256.to_owned())
        );
    }
}
