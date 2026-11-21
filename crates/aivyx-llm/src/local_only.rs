//! 2.0 (local-only) — the guard on every model HTTP client.
//!
//! aivyx-pa only ever contacts models on the operator's own machine or
//! network. Three layers, all here:
//!
//! - [`check_url`] / [`check_url_async`]: aivyx-route's `require_local`
//!   on a URL, run when a provider is built and again before each request
//!   or stream (re)connect. Blocking DNS, so the async form runs it in
//!   `spawn_blocking`.
//! - [`LocalOnlyDns012`]: a reqwest **0.12** connect-time resolver over
//!   `aivyx_route::resolve_local`, so a host name can only ever connect to
//!   local addresses, whatever DNS answers between the URL check and the
//!   connection. (aivyx-route's own `LocalOnlyDns` is for reqwest 0.13.)
//!   reqwest skips the resolver for IP-literal URLs, which is why the
//!   per-request URL check stays too.
//! - [`model_client_builder`]: no proxy, **no redirects** (a local server
//!   answering 307/308 would otherwise make reqwest re-send the prompt to
//!   any host), and the resolver above.

use std::net::SocketAddr;
use std::sync::Arc;

use aivyx_route::{Resolve, SystemResolver};

use crate::LlmError;

/// `Ok` when `url` is local (system resolver). Blocking DNS for a name that
/// isn't local by spelling; on an async path use [`check_url_async`].
pub fn check_url(url: &str) -> Result<(), LlmError> {
    check_url_with(url, &SystemResolver)
}

/// [`check_url`] with an explicit resolver (a test seam).
pub fn check_url_with(url: &str, resolver: &dyn Resolve) -> Result<(), LlmError> {
    aivyx_route::require_local_with(url, resolver).map_err(|e| LlmError::NotLocal(e.to_string()))
}

/// [`check_url`] off the async runtime (`spawn_blocking`, system resolver).
pub async fn require_local_async(url: &str) -> Result<(), LlmError> {
    check_url_async(url, Arc::new(SystemResolver)).await
}

/// [`check_url_with`] off the async runtime (`spawn_blocking`).
pub async fn check_url_async(url: &str, resolver: Arc<dyn Resolve>) -> Result<(), LlmError> {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || check_url_with(&url, &*resolver))
        .await
        .map_err(|e| LlmError::Transport(format!("local-only check did not finish: {e}")))?
}

/// A reqwest 0.12 [`Resolve`](reqwest::dns::Resolve) that returns only local
/// addresses, or the `NotLocal` error (boxed) — see the module docs.
#[derive(Clone)]
pub struct LocalOnlyDns012 {
    resolver: Arc<dyn Resolve>,
}

impl LocalOnlyDns012 {
    /// Over the system resolver.
    pub fn new() -> Self {
        Self::with_resolver(Arc::new(SystemResolver))
    }

    /// Over `resolver` (a test seam).
    pub fn with_resolver(resolver: Arc<dyn Resolve>) -> Self {
        LocalOnlyDns012 { resolver }
    }
}

impl Default for LocalOnlyDns012 {
    fn default() -> Self {
        Self::new()
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The addresses `host` connects to, all local, or the `NotLocal` error.
/// Port 0: reqwest substitutes the URL's port.
fn local_addrs(host: &str, resolver: &dyn Resolve) -> Result<Vec<SocketAddr>, BoxError> {
    aivyx_route::resolve_local_with(host, 0, resolver).map_err(|e| Box::new(e) as BoxError)
}

impl reqwest::dns::Resolve for LocalOnlyDns012 {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let addrs = tokio::task::spawn_blocking(move || local_addrs(&host, &*resolver))
                .await
                .map_err(|e| Box::new(e) as BoxError)??;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The client builder for a model URL: no proxy, no redirects, the
/// local-only connect-time resolver (system DNS).
pub fn model_client_builder() -> reqwest::ClientBuilder {
    model_client_builder_with(Arc::new(SystemResolver))
}

/// [`model_client_builder`] over `resolver` (a test seam).
pub fn model_client_builder_with(resolver: Arc<dyn Resolve>) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(LocalOnlyDns012::with_resolver(resolver)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    use reqwest::dns::Resolve as _;

    /// Fixed answers, no network.
    struct Stub(Vec<IpAddr>);

    impl Resolve for Stub {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    /// Fails the test if a lookup happens at all.
    struct NoLookup;

    impl Resolve for NoLookup {
        fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            panic!("unexpected DNS lookup of {host}");
        }
    }

    fn chain(err: &(dyn std::error::Error + 'static)) -> String {
        let mut text = err.to_string();
        let mut source = err.source();
        while let Some(e) = source {
            text.push_str(" / ");
            text.push_str(&e.to_string());
            source = e.source();
        }
        text
    }

    #[test]
    fn the_url_check_refuses_cloud_and_public_urls_with_the_exact_text() {
        let err = check_url_with("https://api.openai.com/v1", &NoLookup).unwrap_err();
        assert_eq!(
            err.to_string(),
            "https://api.openai.com/v1 is not local (cloud host) — aivyx is local-only; point it \
             at a model server on this machine or your own network"
        );
        assert!(matches!(err, LlmError::NotLocal(_)));
        let err = check_url_with("http://203.0.113.7:8080", &NoLookup).unwrap_err();
        assert!(err.to_string().contains("resolves to a public address 203.0.113.7"), "{err}");
        check_url_with("http://127.0.0.1:8080", &NoLookup).unwrap();
        check_url_with("http://192.168.1.20:11434", &NoLookup).unwrap();
    }

    #[tokio::test]
    async fn the_async_check_uses_the_resolver() {
        let public: Arc<dyn Resolve> = Arc::new(Stub(vec!["8.8.8.8".parse().unwrap()]));
        let err = check_url_async("http://models.example.com:8080", public).await.unwrap_err();
        assert!(err.to_string().contains("public address 8.8.8.8"), "{err}");
        let local: Arc<dyn Resolve> = Arc::new(Stub(vec!["10.0.0.5".parse().unwrap()]));
        check_url_async("http://models.example.com:8080", local).await.unwrap();
    }

    #[tokio::test]
    async fn the_resolver_refuses_a_known_cloud_host_without_a_lookup() {
        // The system-resolver form: a known cloud host is refused before any
        // lookup, so this is offline too.
        let name: reqwest::dns::Name = "api.openai.com".parse().unwrap();
        let err = LocalOnlyDns012::new().resolve(name).await.err().expect("refused");
        let text = chain(&*err);
        assert!(text.contains("is not local") && text.contains("cloud host"), "{text}");
    }

    #[tokio::test]
    async fn a_client_with_the_resolver_never_connects_to_a_cloud_host() {
        let client = model_client_builder().build().unwrap();
        let err = client.get("http://api.openai.com:9/").send().await.unwrap_err();
        let text = chain(&err);
        assert!(text.contains("is not local"), "{text}");
    }

    #[test]
    fn the_resolver_maps_answers_to_local_socket_addresses_or_refuses() {
        let local = Stub(vec!["10.0.0.5".parse().unwrap(), "fd00::1".parse().unwrap()]);
        let addrs = local_addrs("models.example.com", &local).unwrap();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.iter().all(|a| a.port() == 0), "{addrs:?}");

        let mixed = Stub(vec!["10.0.0.5".parse().unwrap(), "93.184.216.34".parse().unwrap()]);
        let err = local_addrs("models.example.com", &mixed).unwrap_err();
        assert!(err.to_string().contains("public address 93.184.216.34"), "{err}");

        let empty = Stub(Vec::new());
        let err = local_addrs("models.example.com", &empty).unwrap_err();
        assert!(err.to_string().contains("doesn't resolve"), "{err}");

        // An IP literal needs no lookup.
        let addrs = local_addrs("127.0.0.1", &NoLookup).unwrap();
        assert_eq!(addrs[0].ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
        assert!(local_addrs("203.0.113.7", &NoLookup).is_err());
    }

    #[tokio::test]
    async fn the_reqwest_resolver_uses_its_seam() {
        let name: reqwest::dns::Name = "models.example.com".parse().unwrap();
        let ok = LocalOnlyDns012::with_resolver(Arc::new(Stub(vec!["192.168.1.9".parse().unwrap()])));
        let addrs: Vec<_> = ok.resolve(name).await.ok().unwrap().collect();
        assert_eq!(addrs, vec!["192.168.1.9:0".parse::<SocketAddr>().unwrap()]);
        let name: reqwest::dns::Name = "models.example.com".parse().unwrap();
        let bad = LocalOnlyDns012::with_resolver(Arc::new(Stub(vec!["1.1.1.1".parse().unwrap()])));
        let err = bad.resolve(name).await.err().unwrap();
        assert!(chain(&*err).contains("is not local"));
    }
}
