//! Bounded package-manager references and command construction.
//!
//! Package operations are intentionally explicit.  The Agent chooses a known
//! package manager and a package/version value that passes a conservative
//! character policy; it never supplies a package-manager flag or a free-form
//! command.  The desktop command layer owns SSH I/O and maps failures onto the
//! host protocol.

use serde::Serialize;

use crate::docker::shell_quote;

/// Default timeout for a package installation.
pub const DEFAULT_PACKAGE_INSTALL_TIMEOUT: usize = 180;
/// Upper bound for an installation request.
pub const MAX_PACKAGE_INSTALL_TIMEOUT: usize = 600;
/// Maximum package-manager output accepted by the normalized parser.
pub const MAX_PACKAGE_INSPECT_CHARS: usize = 4_096;
/// Maximum package/version reference length.
pub const MAX_PACKAGE_REF_CHARS: usize = 128;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageInspectResult {
    pub manager: String,
    pub package: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageInstallResult {
    pub manager: String,
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub installed: bool,
}

/// Only managers with a stable, non-interactive command shape are exposed.
#[must_use]
pub fn is_supported_package_manager(value: &str) -> bool {
    matches!(value, "apt" | "dnf")
}

/// Validate a package name before it enters a remote shell command.
#[must_use]
pub fn is_safe_package_ref(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= MAX_PACKAGE_REF_CHARS
        && first.is_ascii_alphanumeric()
        && chars.all(|character| character.is_ascii_alphanumeric() || "+._:-".contains(character))
}

/// Validate an exact package version supplied by the caller.
#[must_use]
pub fn is_safe_package_version(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= MAX_PACKAGE_REF_CHARS
        && first.is_ascii_alphanumeric()
        && chars.all(|character| character.is_ascii_alphanumeric() || "+._:~^-".contains(character))
}

/// Build a fixed package query.  The command returns one normalized marker:
/// `installed<TAB>version`, `absent`, or `unsupported`.
#[must_use]
pub fn package_inspect_command(manager: &str, package: &str) -> Option<String> {
    if !is_supported_package_manager(manager) || !is_safe_package_ref(package) {
        return None;
    }
    let package = shell_quote(package);
    Some(match manager {
        "apt" => format!(
            "if command -v dpkg-query >/dev/null 2>&1; then dpkg-query --showformat='${{Status}}\\t${{Version}}\\n' --show -- {package} 2>/dev/null || printf '%s\\n' absent; else printf '%s\\n' unsupported; fi"
        ),
        "dnf" => format!(
            "if command -v rpm >/dev/null 2>&1; then rpm --queryformat='installed\\t%{{VERSION}}-%{{RELEASE}}\\n' --query -- {package} 2>/dev/null || printf '%s\\n' absent; else printf '%s\\n' unsupported; fi"
        ),
        _ => return None,
    })
}

/// Build a non-interactive package install command.  Privilege escalation is
/// deliberately not attempted; the connected account must already have the
/// required permission, so a prompt cannot hang a background Agent run.
pub fn package_install_command(
    manager: &str,
    package: &str,
    version: Option<&str>,
) -> Result<String, String> {
    if !is_supported_package_manager(manager) {
        return Err("package manager must be apt or dnf".into());
    }
    if !is_safe_package_ref(package) {
        return Err("package must be a safe package reference".into());
    }
    if let Some(version) = version {
        if !is_safe_package_version(version) {
            return Err("package version contains unsupported characters".into());
        }
    }
    let spec = match (manager, version) {
        ("apt", Some(version)) => format!("{package}={version}"),
        ("dnf", Some(version)) => format!("{package}-{version}"),
        _ => package.to_string(),
    };
    let spec = shell_quote(&spec);
    Ok(match manager {
        "apt" => format!(
            "DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends --no-remove -- {spec}"
        ),
        "dnf" => format!(
            "dnf install --assumeyes --setopt=install_weak_deps=False -- {spec}"
        ),
        _ => unreachable!("manager was validated above"),
    })
}

/// Parse the one-line output emitted by [`package_inspect_command`].
pub fn parse_package_inspect(
    raw: &str,
    manager: &str,
    package: &str,
) -> Result<PackageInspectResult, String> {
    if !is_supported_package_manager(manager) {
        return Err("package manager must be apt or dnf".into());
    }
    if !is_safe_package_ref(package) {
        return Err("package must be a safe package reference".into());
    }
    if raw.chars().count() > MAX_PACKAGE_INSPECT_CHARS {
        return Err("package inspect output exceeded its size limit".into());
    }
    let mut lines = raw.lines().filter(|line| !line.trim().is_empty());
    let line = lines
        .next()
        .ok_or_else(|| "package inspect returned no status".to_string())?
        .trim();
    if lines.next().is_some() {
        return Err("package inspect returned unexpected extra fields".into());
    }
    if line == "unsupported" {
        return Err(format!(
            "package manager {manager} is unavailable on the target"
        ));
    }
    if line == "absent" {
        return Ok(PackageInspectResult {
            manager: manager.to_string(),
            package: package.to_string(),
            installed: false,
            version: None,
        });
    }
    let version = line
        .strip_prefix("installed\t")
        .or_else(|| line.strip_prefix("install ok installed\t"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "package inspect returned an invalid status".to_string())?;
    if version.chars().count() > MAX_PACKAGE_REF_CHARS
        || version.chars().any(|character| character.is_control())
    {
        return Err("package inspect returned an invalid version".into());
    }
    Ok(PackageInspectResult {
        manager: manager.to_string(),
        package: package.to_string(),
        installed: true,
        version: Some(version.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        is_safe_package_ref, is_safe_package_version, package_inspect_command,
        package_install_command, parse_package_inspect,
    };

    #[test]
    fn package_references_and_versions_are_conservative() {
        assert!(is_safe_package_ref("nginx"));
        assert!(is_safe_package_ref("python3.12-dev"));
        assert!(!is_safe_package_ref("-nginx"));
        assert!(!is_safe_package_ref("nginx;rm -rf /"));
        assert!(is_safe_package_version("1:1.27.0~bookworm"));
        assert!(!is_safe_package_version("1.0;touch /tmp/pwned"));
    }

    #[test]
    fn commands_are_fixed_and_shell_safe() {
        assert_eq!(
            package_inspect_command("apt", "nginx").expect("apt query"),
            "if command -v dpkg-query >/dev/null 2>&1; then dpkg-query --showformat='${Status}\\t${Version}\\n' --show -- 'nginx' 2>/dev/null || printf '%s\\n' absent; else printf '%s\\n' unsupported; fi"
        );
        assert_eq!(
            package_install_command("apt", "nginx", Some("1.27.0"))
                .expect("apt install"),
            "DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends --no-remove -- 'nginx=1.27.0'"
        );
        assert_eq!(
            package_install_command("dnf", "nginx", None).expect("dnf install"),
            "dnf install --assumeyes --setopt=install_weak_deps=False -- 'nginx'"
        );
        assert!(package_inspect_command("apk", "nginx").is_none());
    }

    #[test]
    fn inspect_normalizes_installed_absent_and_unsupported_states() {
        let installed = parse_package_inspect("install ok installed\t1.27.0-1\n", "apt", "nginx")
            .expect("installed");
        assert_eq!(installed.version.as_deref(), Some("1.27.0-1"));
        assert!(installed.installed);

        let absent = parse_package_inspect("absent\n", "dnf", "nginx").expect("absent");
        assert!(!absent.installed);
        assert!(absent.version.is_none());

        assert!(parse_package_inspect("unsupported\n", "apt", "nginx").is_err());
        assert!(parse_package_inspect("installed\t1\nextra\n", "apt", "nginx").is_err());
    }
}
