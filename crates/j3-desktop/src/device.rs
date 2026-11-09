//! What this computer is called. A name set in Settings wins; otherwise one is inferred,
//! because the raw hostname is unstable: on a campus network it becomes the DHCP
//! registration's long name (`dhcp-10-0-0-1.netreg.example.edu`) while the machine's own
//! name stays `revolver`.

use std::sync::LazyLock;

/// The name to show for this computer: the one set in Settings, else [`inferred`].
pub fn name(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| inferred().to_owned(), str::to_owned)
}

/// The name inferred from the machine, computed once. On macOS that is the Bonjour
/// `LocalHostName` (what System Settings › Sharing shows, unaffected by the network), then
/// the computer name; elsewhere, and as the fallback, the hostname's first label.
pub fn inferred() -> &'static str {
    static NAME: LazyLock<String> = LazyLock::new(|| {
        platform_name().unwrap_or_else(|| {
            short_label(&gethostname::gethostname().to_string_lossy()).to_owned()
        })
    });
    &NAME
}

#[cfg(target_os = "macos")]
fn platform_name() -> Option<String> {
    ["LocalHostName", "ComputerName"]
        .into_iter()
        .find_map(|key| {
            let out = std::process::Command::new("scutil")
                .args(["--get", key])
                .output()
                .ok()?;
            let name = String::from_utf8(out.stdout).ok()?;
            let name = name.trim();
            (out.status.success() && !name.is_empty()).then(|| name.to_owned())
        })
}

#[cfg(not(target_os = "macos"))]
fn platform_name() -> Option<String> {
    None
}

/// `host` without its domain (`revived.lan` is `revived`); an IPv4 address is kept whole.
fn short_label(host: &str) -> &str {
    let host = host.trim();
    if host.split('.').all(|part| part.parse::<u8>().is_ok()) {
        return host;
    }
    host.split('.').next().unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_name_wins_and_blank_falls_back() {
        assert_eq!(name(Some("  revolver ")), "revolver");
        assert_eq!(name(Some("   ")), inferred());
        assert_eq!(name(None), inferred());
    }

    #[test]
    fn the_domain_is_not_part_of_the_name() {
        assert_eq!(short_label("dhcp-1.netreg.example.edu"), "dhcp-1");
        assert_eq!(short_label("shieldy"), "shieldy");
        assert_eq!(short_label("10.0.0.7"), "10.0.0.7");
    }
}
