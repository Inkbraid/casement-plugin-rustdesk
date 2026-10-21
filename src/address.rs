//! `rustdesk://` addresses.
//!
//! ```text
//! rustdesk://<peer-id>?server=<hbbs-host[:21116]>&key=<server public key>[&relay=<hbbr-host[:21117]>]
//! rustdesk://test[?width=…&height=…&fps=…&password=…&fail=…&close_after=…&resize_every=…]
//! ```
//!
//! Query values are percent-decoded, but `+` is kept as `+` (not a space) so a
//! base64 key can be pasted as it is.

use base64::Engine as _;
use percent_encoding::percent_decode_str;
use std::collections::HashMap;

pub const SCHEME: &str = "rustdesk";
pub const RENDEZVOUS_PORT: u16 = 21116;
pub const RELAY_PORT: u16 = 21117;

#[derive(Debug, Clone, PartialEq)]
pub enum Address {
    /// A peer behind a self-hosted ID server.
    Peer(PeerAddress),
    /// The built-in test pattern.
    Test(TestOptions),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeerAddress {
    pub id: String,
    /// `host:port` of hbbs.
    pub server: String,
    /// `host:port` of hbbr, if given; otherwise the one hbbs names.
    pub relay: Option<String>,
    /// The server's public key, base64, as hbbs prints it (`id_ed25519.pub`).
    pub key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestOptions {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Report `auth_failed` unless `connect` carries this password.
    pub password: Option<String>,
    /// Report `failed` with this text instead of connecting.
    pub fail: Option<String>,
    /// Report `closed` after this many seconds.
    pub close_after: Option<f64>,
    /// Switch between two display sizes every this many seconds.
    pub resize_every: Option<f64>,
}

impl Default for TestOptions {
    fn default() -> Self {
        TestOptions {
            width: 1280,
            height: 720,
            fps: 30,
            password: None,
            fail: None,
            close_after: None,
            resize_every: None,
        }
    }
}

/// Parses an address. Errors are sentences for the user.
pub fn parse(address: &str) -> Result<Address, String> {
    let rest = address
        .split_once("://")
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|(_, rest)| rest)
        .ok_or_else(|| format!("“{address}” isn't a rustdesk:// address."))?;
    let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
    let target = target.trim_end_matches('/');
    let params = parse_query(query)?;

    if target.eq_ignore_ascii_case("test") {
        return parse_test(&params).map(Address::Test);
    }

    let id = percent_decode_str(target)
        .decode_utf8()
        .map_err(|_| "The RustDesk ID isn't valid text.".to_string())?
        .into_owned();
    if id.is_empty() {
        return Err("The address has no RustDesk ID.".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "“{id}” isn't a RustDesk ID (letters, digits, - and _ only)."
        ));
    }

    let server = params
        .get("server")
        .filter(|s| !s.is_empty())
        .ok_or("The address needs server=<your ID server>; this plugin only uses self-hosted RustDesk servers.")?;
    let server = with_default_port(server, RENDEZVOUS_PORT)?;
    let relay = match params.get("relay").filter(|s| !s.is_empty()) {
        Some(r) => Some(with_default_port(r, RELAY_PORT)?),
        None => None,
    };
    let key = params
        .get("key")
        .filter(|s| !s.is_empty())
        .ok_or("The address needs key=<the ID server's public key> so the server can be verified.")?
        .trim()
        .to_string();
    match base64::engine::general_purpose::STANDARD.decode(&key) {
        Ok(k) if k.len() == 32 => {}
        _ => {
            return Err(
                "The server key isn't valid: it should be the 44-character base64 key from the server's id_ed25519.pub."
                    .into(),
            )
        }
    }
    Ok(Address::Peer(PeerAddress {
        id,
        server,
        relay,
        key,
    }))
}

fn parse_query(query: &str) -> Result<HashMap<String, String>, String> {
    let mut params = HashMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let decode = |s: &str| {
            percent_decode_str(s)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| format!("“{pair}” in the address isn't valid text."))
        };
        params.insert(decode(k)?.to_ascii_lowercase(), decode(v)?);
    }
    Ok(params)
}

/// Adds the default port to `host`, `host:port`, `[v6]` or `[v6]:port`.
fn with_default_port(host: &str, port: u16) -> Result<String, String> {
    let host = host.trim();
    let bad = || format!("“{host}” isn't a host name or host:port.");
    if host.is_empty() || host.contains(['/', '?', '#', ' ', '@']) {
        return Err(bad());
    }
    if let Some(rest) = host.strip_prefix('[') {
        let (_, after) = rest.split_once(']').ok_or_else(bad)?;
        return match after {
            "" => Ok(format!("{host}:{port}")),
            p if p
                .strip_prefix(':')
                .is_some_and(|p| p.parse::<u16>().is_ok()) =>
            {
                Ok(host.to_string())
            }
            _ => Err(bad()),
        };
    }
    match host.matches(':').count() {
        0 => Ok(format!("{host}:{port}")),
        1 => {
            let (_, p) = host.split_once(':').ok_or_else(bad)?;
            p.parse::<u16>().map_err(|_| bad())?;
            Ok(host.to_string())
        }
        // A bare IPv6 address.
        _ => Ok(format!("[{host}]:{port}")),
    }
}

fn parse_test(params: &HashMap<String, String>) -> Result<TestOptions, String> {
    let mut o = TestOptions::default();
    let num = |k: &str| -> Result<Option<f64>, String> {
        params
            .get(k)
            .map(|v| {
                v.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .ok_or(format!("{k}={v} isn't a positive number."))
            })
            .transpose()
    };
    if let Some(w) = num("width")? {
        o.width = (w as u32).clamp(16, 8192);
    }
    if let Some(h) = num("height")? {
        o.height = (h as u32).clamp(16, 8192);
    }
    if let Some(f) = num("fps")? {
        o.fps = (f as u32).clamp(1, 240);
    }
    o.close_after = num("close_after")?;
    o.resize_every = num("resize_every")?;
    o.password = params.get("password").cloned();
    o.fail = params.get("fail").cloned();
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "OeVuKk5nlHiXp+APNn0Y3pC1Iwpwn44JGqrQCsWqmBw=";

    fn peer(a: &str) -> PeerAddress {
        match parse(a).unwrap() {
            Address::Peer(p) => p,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn peer_with_defaults() {
        let p = peer(&format!(
            "rustdesk://123456789?server=hbbs.example.com&key={KEY}"
        ));
        assert_eq!(p.id, "123456789");
        assert_eq!(p.server, "hbbs.example.com:21116");
        assert_eq!(p.relay, None);
        assert_eq!(p.key, KEY);
    }

    #[test]
    fn peer_with_ports_relay_and_encoded_key() {
        let enc = KEY.replace('+', "%2B").replace('=', "%3D");
        let p = peer(&format!(
            "RustDesk://my-pc/?key={enc}&server=10.0.0.5:4000&relay=[fd00::1]"
        ));
        assert_eq!(p.id, "my-pc");
        assert_eq!(p.server, "10.0.0.5:4000");
        assert_eq!(p.relay.as_deref(), Some("[fd00::1]:21117"));
        assert_eq!(p.key, KEY);
        assert_eq!(
            peer(&format!("rustdesk://1?server=fd00::2&key={KEY}")).server,
            "[fd00::2]:21116"
        );
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(parse("vnc://host").is_err());
        assert!(parse(&format!("rustdesk://?server=h&key={KEY}")).is_err());
        assert!(parse(&format!("rustdesk://1 2?server=h&key={KEY}")).is_err());
        assert!(parse("rustdesk://123?server=h").is_err());
        assert!(parse(&format!("rustdesk://123?key={KEY}")).is_err());
        assert!(parse("rustdesk://123?server=h&key=abc").is_err());
        assert!(parse(&format!("rustdesk://123?server=h:99999&key={KEY}")).is_err());
    }

    #[test]
    fn test_address() {
        assert_eq!(
            parse("rustdesk://test").unwrap(),
            Address::Test(TestOptions::default())
        );
        let Address::Test(o) =
            parse("rustdesk://test?width=640&height=480&password=pw&close_after=2").unwrap()
        else {
            panic!()
        };
        assert_eq!((o.width, o.height), (640, 480));
        assert_eq!(o.password.as_deref(), Some("pw"));
        assert_eq!(o.close_after, Some(2.0));
        assert!(parse("rustdesk://test?fps=x").is_err());
    }
}
