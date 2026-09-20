//! Host-owned backup path construction and validation.
//!
//! A model supplies only the source path. The desktop host supplies a random hexadecimal token,
//! and this module derives a sibling path that can later be accepted by filesystem.restore only
//! for that same source path. This keeps recovery useful without allowing an arbitrary copy or
//! restore destination.

use crate::limits::{BACKUP_TOKEN_CHARS, MAX_REMOTE_PATH_CHARS};
use crate::policy::validate_remote_path;
use crate::revision::content_revision;

/// Marker kept in the remote filename so a recovery path is visibly tool-owned.
pub const BACKUP_MARKER: &str = ".yukinal-backup-";

/// Validate the host-generated backup token.
#[must_use]
pub fn is_safe_backup_token(token: &str) -> bool {
    token.chars().count() == BACKUP_TOKEN_CHARS
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Derive the only path where a source file may be backed up.
pub fn backup_path_for(path: &str, token: &str) -> Result<String, String> {
    validate_remote_path(path)?;
    if !is_safe_backup_token(token) {
        return Err(format!(
            "backup token must be {BACKUP_TOKEN_CHARS} lowercase hexadecimal characters"
        ));
    }
    let Some((parent, name)) = path.rsplit_once('/') else {
        return Err("backup source must name a file below the remote root".to_string());
    };
    if name.is_empty() {
        return Err("backup source must name a file, not a directory".to_string());
    }
    let parent = if parent.is_empty() { "/" } else { parent };
    let source_fingerprint = content_revision(path.as_bytes());
    let backup_name = format!("{BACKUP_MARKER}{}-{token}", &source_fingerprint[..16]);
    let backup_path = if parent == "/" {
        format!("/{backup_name}")
    } else {
        format!("{parent}/{backup_name}")
    };
    if backup_path.chars().count() > MAX_REMOTE_PATH_CHARS {
        return Err(format!(
            "derived backup path must be at most {MAX_REMOTE_PATH_CHARS} characters"
        ));
    }
    Ok(backup_path)
}

/// Return whether a backup path is derived for exactly the source path.
#[must_use]
pub fn is_backup_path_for(path: &str, backup_path: &str) -> bool {
    let Ok(expected_prefix_path) = backup_path_for(path, &"0".repeat(BACKUP_TOKEN_CHARS)) else {
        return false;
    };
    if validate_remote_path(backup_path).is_err() {
        return false;
    }
    let Some((parent, name)) = expected_prefix_path.rsplit_once('/') else {
        return false;
    };
    let Some((actual_parent, actual_name)) = backup_path.rsplit_once('/') else {
        return false;
    };
    let zeros = "0".repeat(BACKUP_TOKEN_CHARS);
    let prefix = name.strip_suffix(&zeros).unwrap_or(name);
    actual_parent == parent
        && actual_name.starts_with(prefix)
        && actual_name.len() == prefix.len() + BACKUP_TOKEN_CHARS
        && is_safe_backup_token(&actual_name[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::{backup_path_for, is_backup_path_for, is_safe_backup_token, BACKUP_MARKER};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn derives_a_sibling_path_without_accepting_a_destination() {
        let backup = backup_path_for("/etc/yukinal.conf", TOKEN).expect("backup path");
        assert!(backup.starts_with("/etc/"));
        assert!(backup.contains(BACKUP_MARKER));
        assert!(is_backup_path_for("/etc/yukinal.conf", &backup));
        assert!(!is_backup_path_for("/etc/other.conf", &backup));
        assert!(!is_backup_path_for("/var/yukinal.conf", &backup));
    }

    #[test]
    fn root_files_and_tokens_are_bounded() {
        let backup = backup_path_for("/config", TOKEN).expect("root backup path");
        assert!(backup.starts_with("/.yukinal-backup-"));
        assert!(is_safe_backup_token(TOKEN));
        assert!(!is_safe_backup_token("0123456789abcdef0123456789abcdeG"));
        assert!(!is_safe_backup_token("short"));
        assert!(backup_path_for("/config/", TOKEN).is_err());
        assert!(backup_path_for("relative", TOKEN).is_err());
        assert!(backup_path_for("/config", "0123;id").is_err());
    }

    #[test]
    fn arbitrary_backup_names_are_rejected() {
        assert!(!is_backup_path_for(
            "/etc/yukinal.conf",
            "/etc/.yukinal-backup-aaaaaaaaaaaaaaaa-0123456789abcdef0123456789abcdef/extra"
        ));
        assert!(!is_backup_path_for(
            "/etc/yukinal.conf",
            "/etc/not-a-yukinal-backup-0123456789abcdef-0123456789abcdef0123456789abcdef"
        ));
    }
}
