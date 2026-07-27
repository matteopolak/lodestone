//! Server address resolution: SRV records and the vanilla connect rules.
//!
//! Real Minecraft servers are very often reachable only through a DNS `SRV`
//! record (`_minecraft._tcp.<host>`) that redirects the advertised hostname to a
//! different host and/or port. A client that only ever dials `host:25565` cannot
//! reach a large fraction of public servers — the same *class* of gap as
//! encryption: invisible in a lab, fatal in the field.
//!
//! The Notchian client's rule is: perform the SRV lookup **only when the user
//! did not specify an explicit port** (and the host is a name, not an IP
//! literal). If a record is found, connect to its target and port; otherwise
//! fall back to the host with the default port.
//!
//! The *policy* here — when to query and how to interpret the result — is pure
//! and exhaustively unit-tested. The actual DNS I/O lives behind it and is
//! exercised by an `#[ignore]`d live test.

use std::net::IpAddr;

#[cfg(not(target_arch = "wasm32"))]
use crate::error::{NetError, Result};

/// The default Minecraft server port, used when neither an explicit port nor an
/// SRV record supplies one.
pub const DEFAULT_PORT: u16 = 25565;

/// A concrete host and port to open a TCP connection to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAddress {
    /// Target host (a name or IP literal).
    pub host: String,
    /// Target port.
    pub port: u16,
}

impl ResolvedAddress {
    /// Renders the address as `host:port` for use with a socket connect.
    #[must_use]
    pub fn socket_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Returns whether an SRV lookup should be attempted for `host` given an
/// optional explicit port.
///
/// Vanilla skips SRV when the user pinned a port, and it is pointless for an IP
/// literal (which cannot own an SRV record).
#[must_use]
pub fn should_query_srv(host: &str, explicit_port: Option<u16>) -> bool {
    explicit_port.is_none() && host.parse::<IpAddr>().is_err()
}

/// The DNS name to query for a Minecraft SRV record for `host`.
#[must_use]
pub fn srv_query_name(host: &str) -> String {
    format!("_minecraft._tcp.{host}")
}

/// Chooses the final address from the host, an optional explicit port, and an
/// optional SRV result `(target, port)`.
///
/// Precedence: an explicit port always wins (and suppresses any SRV result); a
/// present SRV result is used next; otherwise the host with [`DEFAULT_PORT`].
#[must_use]
pub fn choose_address(
    host: &str,
    explicit_port: Option<u16>,
    srv: Option<(String, u16)>,
) -> ResolvedAddress {
    if let Some(port) = explicit_port {
        return ResolvedAddress {
            host: host.to_owned(),
            port,
        };
    }
    if let Some((target, port)) = srv {
        return ResolvedAddress { host: target, port };
    }
    ResolvedAddress {
        host: host.to_owned(),
        port: DEFAULT_PORT,
    }
}

/// Resolves `host` (with an optional explicit `port`) to a concrete address,
/// consulting DNS for an SRV record when the vanilla rules call for it.
///
/// A missing SRV record is not an error — it is the common case, and we fall
/// back to the host with the default port. Only when the vanilla rules say to
/// query and the lookup is attempted do resolver failures other than
/// "no record" surface; here we treat any lookup failure as "no record" so a
/// broken or SRV-less zone still connects on the default port, matching vanilla.
///
/// # Errors
///
/// Returns [`NetError::Dns`] only if the resolver itself cannot be constructed
/// from the system configuration.
#[cfg(not(target_arch = "wasm32"))]
pub async fn resolve_server_address(host: &str, port: Option<u16>) -> Result<ResolvedAddress> {
    if !should_query_srv(host, port) {
        return Ok(choose_address(host, port, None));
    }
    let srv = lookup_minecraft_srv(host).await?;
    Ok(choose_address(host, port, srv))
}

/// Performs the actual SRV DNS lookup, returning the highest-priority record's
/// `(target, port)` if any.
///
/// # Errors
///
/// Returns [`NetError::Dns`] if the system resolver cannot be built. A lookup
/// that finds no record (or fails to reach a server) yields `Ok(None)` so the
/// caller can fall back to the default port.
#[cfg(not(target_arch = "wasm32"))]
pub async fn lookup_minecraft_srv(host: &str) -> Result<Option<(String, u16)>> {
    use hickory_resolver::Resolver;

    let name = srv_query_name(host);
    let resolver = Resolver::builder_tokio()
        .and_then(|b| b.build())
        .map_err(|e| NetError::Dns {
            name: name.clone(),
            reason: e.to_string(),
        })?;

    match resolver.srv_lookup(name).await {
        Ok(lookup) => {
            use hickory_resolver::proto::rr::RData;
            let found = lookup.answers().iter().find_map(|rec| {
                if let RData::SRV(srv) = &rec.data {
                    // The SRV target is a fully-qualified name with a trailing
                    // dot; strip it for use as a connect host.
                    let target = srv.target.to_utf8();
                    let target = target.strip_suffix('.').unwrap_or(&target).to_owned();
                    Some((target, srv.port))
                } else {
                    None
                }
            });
            Ok(found)
        }
        // No record, NXDOMAIN, or an unreachable resolver all mean "no SRV" for
        // our purposes; the caller falls back to the default port.
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_srv_when_port_is_explicit() {
        assert!(!should_query_srv("example.com", Some(25565)));
        assert!(!should_query_srv("example.com", Some(30000)));
    }

    #[test]
    fn skips_srv_for_ip_literals() {
        assert!(!should_query_srv("127.0.0.1", None));
        assert!(!should_query_srv("::1", None));
        assert!(!should_query_srv("192.168.1.10", None));
    }

    #[test]
    fn queries_srv_for_a_bare_hostname() {
        assert!(should_query_srv("mc.example.com", None));
        assert_eq!(
            srv_query_name("mc.example.com"),
            "_minecraft._tcp.mc.example.com"
        );
    }

    #[test]
    fn explicit_port_beats_srv_and_default() {
        let a = choose_address(
            "mc.example.com",
            Some(25566),
            Some(("srv.host".into(), 12345)),
        );
        assert_eq!(a.host, "mc.example.com");
        assert_eq!(a.port, 25566);
    }

    #[test]
    fn srv_result_used_when_no_explicit_port() {
        let a = choose_address("mc.example.com", None, Some(("srv.host".into(), 12345)));
        assert_eq!(a.host, "srv.host");
        assert_eq!(a.port, 12345);
    }

    #[test]
    fn falls_back_to_default_port_without_srv() {
        let a = choose_address("mc.example.com", None, None);
        assert_eq!(a.host, "mc.example.com");
        assert_eq!(a.port, DEFAULT_PORT);
    }

    #[test]
    fn socket_addr_renders_host_and_port() {
        let a = choose_address("h", Some(25), None);
        assert_eq!(a.socket_addr(), "h:25");
    }
}
