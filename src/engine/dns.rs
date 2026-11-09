//! Bounded multi-address DNS ranking. The resolver always returns every address
//! to Reqwest: TCP racing is only a *ranking* probe, not a replacement for
//! the transport's own IPv4/IPv6 fallback, TLS validation or proxy handling.
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{collections::HashMap, net::{IpAddr, SocketAddr}, sync::{Arc, Mutex}, time::{Duration, Instant}};

const RACE_BUDGET: Duration = Duration::from_secs(3);
const HEAD_START: Duration = Duration::from_millis(250);
const WINNER_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct ResolverState {
    https_hosts: HashMap<String, u16>,
    winners: HashMap<(String, u16), (IpAddr, Instant)>,
}

#[derive(Clone, Default)]
pub(super) struct RacingResolver {
    state: Arc<Mutex<ResolverState>>,
}

impl RacingResolver {
    pub(super) fn new() -> Self { Self::default() }

    pub(super) fn remember_url(&self, raw: &str) {
        if std::env::var("QDM_DNS_RACE").ok().as_deref() == Some("0") { return; }
        let Ok(url) = url::Url::parse(raw) else { return; };
        // HTTP, private targets and unexpected proxy endpoints keep Reqwest's
        // normal resolver. Only explicit HTTPS origins are eligible.
        if url.scheme() != "https" { return; }
        let Some(host) = url.host_str() else { return; };
        if host.parse::<IpAddr>().is_ok() || host.eq_ignore_ascii_case("localhost")
            || host.ends_with(".local") { return; }
        let Some(port) = url.port_or_known_default() else { return; };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.https_hosts.len() > 1024 { state.https_hosts.clear(); }
        state.https_hosts.insert(host.to_ascii_lowercase(), port);
    }

    fn port_and_cached(&self, host: &str) -> (Option<u16>, Option<IpAddr>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let port = state.https_hosts.get(&host.to_ascii_lowercase()).copied();
        let cached = port.and_then(|port| state.winners.get(&(host.to_ascii_lowercase(), port)))
            .and_then(|(ip, at)| (at.elapsed() < WINNER_TTL).then_some(*ip));
        (port, cached)
    }

    fn save_winner(&self, host: &str, port: u16, ip: IpAddr) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.winners.retain(|_, (_, at)| at.elapsed() < WINNER_TTL);
        if state.winners.len() > 1024 { state.winners.clear(); }
        state.winners.insert((host.to_ascii_lowercase(), port), (ip, Instant::now()));
    }
}

fn public_address(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !ip.is_private() && !ip.is_loopback() && !ip.is_link_local()
            && !ip.is_multicast() && !ip.is_broadcast() && !ip.is_unspecified(),
        IpAddr::V6(ip) => !ip.is_loopback() && !ip.is_unique_local()
            && !ip.is_unicast_link_local() && !ip.is_multicast() && !ip.is_unspecified(),
    }
}

async fn race_addresses(ips: &[IpAddr], port: u16) -> Option<IpAddr> {
    if ips.len() < 2 { return None; }
    let (tx, mut rx) = tokio::sync::mpsc::channel(ips.len());
    let mut jobs = tokio::task::JoinSet::new();
    for (index, ip) in ips.iter().copied().take(8).enumerate() {
        let tx = tx.clone();
        jobs.spawn(async move {
            tokio::time::sleep(HEAD_START.saturating_mul(index as u32)).await;
            if tokio::time::timeout(Duration::from_secs(2),
                tokio::net::TcpStream::connect(SocketAddr::new(ip, port))).await
                .is_ok_and(|result| result.is_ok()) {
                let _ = tx.send(ip).await;
            }
        });
    }
    drop(tx);
    let winner = tokio::time::timeout(RACE_BUDGET, rx.recv()).await.ok().flatten();
    jobs.abort_all();
    winner
}

impl Resolve for RacingResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let resolver = self.clone();
        Box::pin(async move {
            let mut addresses: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), 0u16)).await?
                .map(|addr| addr.ip()).collect();
            addresses.dedup();
            let (port, cached) = resolver.port_and_cached(&host);
            if let Some(port) = port {
                // No extra socket probes for private/loopback destinations.
                if addresses.len() > 1 && addresses.iter().all(public_address) {
                    let winner = match cached.filter(|ip| addresses.contains(ip)) {
                        Some(ip) => Some(ip),
                        None => race_addresses(&addresses, port).await,
                    };
                    if let Some(ip) = winner {
                        resolver.save_winner(&host, port, ip);
                        addresses.sort_by_key(|candidate| usize::from(*candidate != ip));
                    }
                }
            }
            Ok(Box::new(addresses.into_iter().map(|ip| SocketAddr::new(ip, 0))) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn multi_ip_race_prefers_reachable_address_and_single_address_is_not_raced() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let unreachable: IpAddr = "127.0.0.2".parse().unwrap();
        let reachable: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(race_addresses(&[reachable], port).await, None);
        assert_eq!(race_addresses(&[unreachable, reachable], port).await, Some(reachable));
    }

    #[test]
    fn winner_cache_is_host_scoped_and_can_be_disabled() {
        let resolver = RacingResolver::new();
        resolver.remember_url("https://cdn.example/download");
        resolver.save_winner("cdn.example", 443, "1.1.1.1".parse().unwrap());
        assert_eq!(resolver.port_and_cached("cdn.example"), (Some(443), Some("1.1.1.1".parse().unwrap())));
        assert_eq!(resolver.port_and_cached("different.example"), (None, None));
        resolver.remember_url("http://localhost:8000/test");
        assert_eq!(resolver.port_and_cached("localhost"), (None, None));
    }
}
