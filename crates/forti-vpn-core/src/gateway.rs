//! Translate the editor's server/port fields to FortiClient's gateway value.
//!
//! A port of the Python 0.1.0 `forti_vpn/gateway.py`. Internationalized
//! hostnames are validated through their punycode form (see
//! `crate::punycode`) but kept as typed.
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::error::VpnError;
use crate::punycode;

const DEFAULT_PORT: &str = "443";
/// The longest hostname in ASCII form, without the trailing dot.
const HOSTNAME_LIMIT: usize = 253;
const LABEL_LIMIT: usize = 63;
const ACE_PREFIX: &str = "xn--";
/// IDNA 2003 label separators (RFC 3490 section 3.1).
const IDNA_DOTS: [char; 4] = ['.', '\u{3002}', '\u{FF0E}', '\u{FF61}'];

const MISSING_SERVER: &str = "Enter a remote gateway hostname or IP address.";
const INVALID_PORT: &str = "Enter a port between 1 and 65535.";
const CONFLICTING_PORT: &str = "Use the Port field to set the remote gateway port.";
const INVALID_SERVER: &str = "Enter a hostname or IP address without a URL, spaces, or path.";

/// Split `host`, `host:port`, `[v6]`, or `[v6]:port` into server and port;
/// the port defaults to `443`. A bare IPv6 address has no port.
pub fn split_gateway(value: &str) -> (String, String) {
    let (server, port) = split(value);
    (server.to_owned(), port.to_owned())
}

/// Build a gateway value from the editor fields. Returns `original`
/// unchanged when it already denotes the same server and port, so editing a
/// profile never rewrites a value such as `vpn.example.org:0443`.
pub fn build_gateway(server: &str, port: &str, original: &str) -> Result<String, VpnError> {
    let server = trim(server);
    if server.is_empty() {
        return Err(VpnError::failed(MISSING_SERVER));
    }
    let port = parse_port(trim(port))
        .ok_or_else(|| VpnError::failed(INVALID_PORT))?
        .to_string();

    // Accept a pasted host:port, but never silently override a different port.
    let (host, pasted_port) = split(server);
    let (server, port) = if host == server {
        (server, port)
    } else {
        if port != DEFAULT_PORT && port != pasted_port {
            return Err(VpnError::failed(CONFLICTING_PORT));
        }
        if parse_port(pasted_port).is_none() {
            return Err(VpnError::failed(INVALID_PORT));
        }
        (host, pasted_port.to_owned())
    };
    if !is_ip_address(server) && !is_hostname(server) {
        return Err(VpnError::failed(INVALID_SERVER));
    }

    if !original.is_empty() {
        let (original_server, original_port) = split(original);
        if server == original_server && port == without_leading_zeros(original_port) {
            return Ok(original.to_owned());
        }
    }
    let host = if server.contains(':') {
        format!("[{server}]")
    } else {
        server.to_owned()
    };
    Ok(if port == DEFAULT_PORT {
        host
    } else {
        format!("{host}:{port}")
    })
}

fn split(value: &str) -> (&str, &str) {
    if let Some(parts) = split_bracketed(value) {
        return parts;
    }
    if value.matches(':').count() == 1
        && let Some((server, port)) = value.split_once(':')
        && is_digits(port)
    {
        return (server, port);
    }
    (value, DEFAULT_PORT)
}

/// `[host]` or `[host]:port`, the Python pattern `\[([^]]+)\](?::([0-9]+))?`.
fn split_bracketed(value: &str) -> Option<(&str, &str)> {
    let (host, rest) = value.strip_prefix('[')?.split_once(']')?;
    if host.is_empty() {
        return None;
    }
    match rest.strip_prefix(':') {
        None if rest.is_empty() => Some((host, DEFAULT_PORT)),
        Some(port) if is_digits(port) => Some((host, port)),
        _ => None,
    }
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// A decimal port in `1..=65535`; leading zeros are allowed.
fn parse_port(value: &str) -> Option<u16> {
    // Check the digits first: `u16::from_str` also accepts a leading `+`.
    if !is_digits(value) {
        return None;
    }
    value.parse().ok().filter(|&port| port != 0)
}

/// Python's `str(int(digits))`.
fn without_leading_zeros(digits: &str) -> &str {
    match digits.trim_start_matches('0') {
        "" => "0",
        trimmed => trimmed,
    }
}

/// Python's `str.strip()`, which also treats the ASCII information
/// separators U+001C..U+001F as whitespace.
fn trim(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || ('\u{1C}'..='\u{1F}').contains(&c))
}

/// Python's `ipaddress.ip_address()`: dotted-quad IPv4, or IPv6 with an
/// optional non-empty `%zone`.
fn is_ip_address(value: &str) -> bool {
    if value.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    if value.contains('/') {
        return false;
    }
    let address = match value.split_once('%') {
        None => value,
        Some((address, zone)) if !zone.is_empty() && !zone.contains('%') => address,
        Some(_) => return false,
    };
    address.parse::<Ipv6Addr>().is_ok()
}

/// Python's check of `server.rstrip('.').encode('idna')`: every label of the
/// ASCII form is letters, digits, and inner hyphens, at most 63 characters,
/// and the whole name is at most 253 characters.
fn is_hostname(server: &str) -> bool {
    ascii_hostname(server.trim_end_matches('.')).is_some_and(|name| {
        !name.is_empty() && name.len() <= HOSTNAME_LIMIT && name.split('.').all(is_ldh_label)
    })
}

/// IDNA 2003 ToASCII for each label. An all-ASCII name is used as typed,
/// like Python's fast path.
fn ascii_hostname(name: &str) -> Option<String> {
    if name.is_ascii() {
        return Some(name.to_owned());
    }
    let labels = name
        .split(IDNA_DOTS)
        .map(ascii_label)
        .collect::<Option<Vec<_>>>()?;
    Some(labels.join("."))
}

/// One label of ToASCII (RFC 3490 section 4.1). Nameprep is reduced to the
/// steps that matter for typed hostnames: invisible characters are dropped,
/// fullwidth ASCII is narrowed, and prohibited characters are refused. So
/// `ｖｐｎ` passes while a fullwidth URL or a pasted no-break space fails, as
/// in Python. Other NFKC mappings, case folding beyond lowercase, and the
/// bidirectional rule are not applied.
fn ascii_label(label: &str) -> Option<String> {
    if label.is_ascii() {
        return Some(label.to_owned());
    }
    let prepared: String = label
        .chars()
        .filter(|&c| !is_mapped_to_nothing(c))
        .map(narrow)
        .collect();
    if prepared.chars().any(is_prohibited) {
        return None;
    }
    if prepared.is_ascii() {
        return Some(prepared);
    }
    // A label that still needs encoding must not pose as an encoded one.
    if prepared
        .get(..ACE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(ACE_PREFIX))
    {
        return None;
    }
    // Encoding emits at least one character per code point, so a longer
    // label cannot fit; skip the quadratic encoder for pasted text.
    if prepared.chars().count() > LABEL_LIMIT - ACE_PREFIX.len() {
        return None;
    }
    punycode::label_to_ascii(&prepared)
}

/// RFC 3454 table B.1: the soft hyphen, joiners, variation selectors, and
/// other invisible characters that nameprep removes.
fn is_mapped_to_nothing(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{34F}'
            | '\u{1806}'
            | '\u{180B}'..='\u{180D}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
    )
}

/// NFKC maps fullwidth forms U+FF01..U+FF5E to ASCII U+0021..U+007E.
fn narrow(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

/// What nameprep prohibits after mapping (RFC 3454 tables C.1.2 to C.9):
/// spaces, control and format characters, private use, non-characters,
/// and invisible direction and tag characters.
fn is_prohibited(c: char) -> bool {
    let non_character = (u32::from(c) & 0xFFFE) == 0xFFFE;
    c.is_whitespace()
        || c.is_control()
        || non_character
        || matches!(
            c,
            '\u{6DD}'
                | '\u{70F}'
                | '\u{180E}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2061}'..='\u{2063}'
                | '\u{206A}'..='\u{206F}'
                | '\u{2FF0}'..='\u{2FFB}'
                | '\u{E000}'..='\u{F8FF}'
                | '\u{FDD0}'..='\u{FDEF}'
                | '\u{FFF9}'..='\u{FFFD}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
                | '\u{F0000}'..='\u{FFFFD}'
                | '\u{100000}'..='\u{10FFFD}'
        )
}

/// `[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?`
fn is_ldh_label(label: &str) -> bool {
    let bytes = label.as_bytes();
    let ends_alphanumeric = matches!(
        (bytes.first(), bytes.last()),
        (Some(first), Some(last)) if first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
    );
    ends_alphanumeric
        && bytes.len() <= LABEL_LIMIT
        && bytes
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(server: &str, port: &str) -> Result<String, VpnError> {
        build_gateway(server, port, "")
    }

    #[test]
    fn existing_gateway_round_trip() {
        for value in [
            "vpn.example.org",
            "vpn.example.org:443",
            "vpn.example.org:0443",
            "vpn.example.org:10443",
            "192.0.2.4:8443",
            "[2001:db8::1]:8443",
            "2001:db8::1",
        ] {
            let (server, port) = split_gateway(value);
            assert_eq!(
                build_gateway(&server, &port, value).as_deref(),
                Ok(value),
                "{value}"
            );
        }
    }

    #[test]
    fn custom_port_and_ipv6() {
        assert_eq!(
            build("vpn.example.org", "10443").as_deref(),
            Ok("vpn.example.org:10443")
        );
        assert_eq!(
            build("2001:db8::1", "8443").as_deref(),
            Ok("[2001:db8::1]:8443")
        );
        assert_eq!(
            build("vpn.example.org:8443", "443").as_deref(),
            Ok("vpn.example.org:8443")
        );
    }

    #[test]
    fn invalid_input_and_conflicting_ports() {
        for (host, port) in [
            ("", "443"),
            ("https://vpn.example.org", "443"),
            ("vpn.example.org/path", "443"),
            ("bad host", "443"),
            ("vpn.example.org", "0"),
            ("vpn.example.org", "65536"),
            ("vpn.example.org", "abc"),
            ("vpn.example.org", "٤٤٣"),
            ("vpn.example.org:8443", "10443"),
        ] {
            assert!(build(host, port).is_err(), "{host:?} {port:?}");
        }
    }

    #[test]
    fn error_messages_match_the_python_app() {
        let message = |server: &str, port: &str| build(server, port).unwrap_err().to_string();
        assert_eq!(message(" ", "443"), MISSING_SERVER);
        assert_eq!(message("vpn.example.org", "+443"), INVALID_PORT);
        assert_eq!(message("vpn.example.org:0", "443"), INVALID_PORT);
        assert_eq!(message("vpn.example.org:8443", "10443"), CONFLICTING_PORT);
        assert_eq!(message("vpn.example.org/path", "443"), INVALID_SERVER);
    }

    #[test]
    fn split_forms() {
        for (value, server, port) in [
            ("vpn.example.org", "vpn.example.org", "443"),
            ("vpn.example.org:0443", "vpn.example.org", "0443"),
            ("vpn.example.org:", "vpn.example.org:", "443"),
            ("vpn.example.org:٤٤٣", "vpn.example.org:٤٤٣", "443"),
            ("[2001:db8::1]", "2001:db8::1", "443"),
            ("[2001:db8::1]:8443", "2001:db8::1", "8443"),
            ("[2001:db8::1]:", "[2001:db8::1]:", "443"),
            ("[]:443", "[]", "443"),
            ("[vpn.example.org]:8443", "vpn.example.org", "8443"),
            ("2001:db8::1", "2001:db8::1", "443"),
            ("https://vpn.example.org", "https://vpn.example.org", "443"),
        ] {
            assert_eq!(
                split_gateway(value),
                (server.to_owned(), port.to_owned()),
                "{value}"
            );
        }
    }

    #[test]
    fn values_are_normalized_like_python() {
        for (server, port, expected) in [
            ("  vpn.example.org  ", " 10443 ", "vpn.example.org:10443"),
            ("\u{1F}vpn.example.org\u{1C}", "443", "vpn.example.org"),
            ("\u{A0}vpn.example.org\u{3000}", "443", "vpn.example.org"),
            ("vpn.example.org", "0443", "vpn.example.org"),
            ("vpn.example.org", "08443", "vpn.example.org:8443"),
            ("vpn.example.org:443", "443", "vpn.example.org"),
            ("vpn.example.org:08443", "443", "vpn.example.org:08443"),
            ("vpn.example.org.", "443", "vpn.example.org."),
            ("[2001:db8::1]", "443", "[2001:db8::1]"),
            ("[vpn.example.org]:8443", "443", "vpn.example.org:8443"),
            ("192.0.2.04", "443", "192.0.2.04"),
            ("::ffff:192.0.2.1", "443", "[::ffff:192.0.2.1]"),
            ("fe80::1%eth0", "8443", "[fe80::1%eth0]:8443"),
        ] {
            assert_eq!(
                build(server, port).as_deref(),
                Ok(expected),
                "{server:?} {port:?}"
            );
        }
    }

    #[test]
    fn original_is_kept_only_for_the_same_server_and_port() {
        for (server, port, original, expected) in [
            (
                "vpn.example.org",
                "443",
                "vpn.example.org:443",
                "vpn.example.org:443",
            ),
            (
                "vpn.example.org",
                "443",
                "vpn.example.org:00443",
                "vpn.example.org:00443",
            ),
            (
                "vpn.example.org:0443",
                "443",
                "vpn.example.org:0443",
                "vpn.example.org:0443",
            ),
            (
                "vpn.example.org",
                "443",
                "vpn.example.org:10443",
                "vpn.example.org",
            ),
            ("2001:db8::1", "443", "[2001:db8::1]", "[2001:db8::1]"),
            ("2001:db8::1", "443", "[2001:db8::1]:8443", "[2001:db8::1]"),
            (
                "other.example.org",
                "443",
                "vpn.example.org",
                "other.example.org",
            ),
        ] {
            assert_eq!(
                build_gateway(server, port, original).as_deref(),
                Ok(expected),
                "{server:?} {port:?} {original:?}"
            );
        }
    }

    #[test]
    fn ip_addresses_follow_python_ipaddress() {
        for valid in [
            "0.0.0.0",
            "::",
            "1:2:3:4:5:6:7::",
            "::1:2:3:4:5:6:7",
            "1:2:3:4:5:6:1.2.3.4",
            "fe80::1%eth 0",
        ] {
            assert!(is_ip_address(valid), "{valid}");
        }
        for invalid in [
            "192.0.2.04",
            "1.2.3",
            "1:2:3:4::5:6:7:8",
            "1:2:3:4:5:6:7:1.2.3.4",
            "fe80::1%",
            "fe80::1%a%b",
            "fe80::1%eth0/64",
            "2001:db8::1/64",
            "12345::1",
        ] {
            assert!(!is_ip_address(invalid), "{invalid}");
        }
    }

    #[test]
    fn hostname_limits() {
        let label = "a".repeat(63);
        assert!(build(&format!("{label}.example.org"), "443").is_ok());
        assert!(build(&format!("a{label}.example.org"), "443").is_err());
        let longest = format!("{label}.{label}.{label}.{}", "b".repeat(61));
        assert_eq!(longest.len(), 253);
        assert!(build(&longest, "443").is_ok());
        assert!(build(&format!("{longest}."), "443").is_ok());
        assert!(build(&format!("{longest}b"), "443").is_err());
        for invalid in [
            ".",
            "...",
            "under_score.example.org",
            "-lead.example.org",
            "trail-.example.org",
        ] {
            assert!(build(invalid, "443").is_err(), "{invalid}");
        }
    }

    #[test]
    fn internationalized_hostnames_are_kept_as_typed() {
        for server in [
            "vpn.bücher.de",
            "VPN.BÜCHER.DE",
            "日本語.jp",
            "vpn。example。com",
            "vpn．example．com",
            "vpn｡example｡com",
            "ｖｐｎ．ｅｘａｍｐｌｅ．ｃｏｍ",
            "ＶＰＮ.example.com",
            "straße.de",
            "☃.net",
            "vpn\u{200B}.example.com",
            "vpn\u{AD}.example.com",
            "\u{212A}m.example.com",
            "xn--\u{200B}-1.example.com",
        ] {
            assert_eq!(build(server, "443").as_deref(), Ok(server));
        }
        assert_eq!(
            build("vpn.bücher.de", "8443").as_deref(),
            Ok("vpn.bücher.de:8443")
        );
        assert!(build(&format!("{}.de", "ü".repeat(30)), "443").is_ok());
        assert!(build(&format!("{}.de", "ü".repeat(60)), "443").is_err());
        // Labels whose encoded form is exactly 63 and 64 characters long.
        for (label, valid) in [
            ("ü".repeat(57), true),
            ("ü".repeat(58), false),
            (format!("{}ü", "a".repeat(55)), true),
            (format!("{}ü", "a".repeat(56)), false),
        ] {
            assert_eq!(
                build(&format!("{label}.de"), "443").is_ok(),
                valid,
                "{label}"
            );
        }
        let pasted: String = (0x4E00..0x4E00 + 50_000)
            .filter_map(char::from_u32)
            .collect();
        assert!(build(&pasted, "443").is_err());
    }

    #[test]
    fn internationalized_hostnames_reject_what_nameprep_rejects() {
        for server in [
            "bücher。",
            "a..bücher",
            "xn--bü.de",
            "XN--bü.de",
            "ｈｔｔｐｓ：／／ｖｐｎ．ｅｘａｍｐｌｅ．ｃｏｍ",
            "ｃｏｍ／ｐａｔｈ.example",
            "vpn\u{200E}.example.com",
            "vpn\u{200F}ü.example.com",
            "vpn\u{202A}ü.de",
            "vpn\u{2028}ü.de",
            "vpn\u{85}.de",
            "vpn\u{1680}.de",
            "vpn\u{A0}x.example.com",
            "vpn\u{3000}x.example.com",
            "bü cher.de",
            "bü\u{1}cher.de",
            "ü/x.de",
            "ü:x.de",
            "\u{200B}.example.com",
            "vpn\u{2061}ü.de",
            "vpn\u{E000}.de",
            "vpn\u{FDD0}.de",
            "vpn\u{1FFFE}.de",
            "vpn\u{E0041}.de",
        ] {
            assert!(build(server, "443").is_err(), "{server:?}");
        }
    }
}
