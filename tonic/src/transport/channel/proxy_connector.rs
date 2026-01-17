use http::Uri;
use hyper_util::client::legacy::connect::{proxy::Tunnel, HttpConnector};
use std::{
    env,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower_service::Service;

/// A connector that conditionally applies proxy settings.
///
/// This wraps an HttpConnector and applies proxy tunneling when configured.
pub(crate) struct ProxyConnector {
    /// The underlying HTTP connector
    http: HttpConnector,
    /// Explicit proxy URI (takes precedence over environment)
    proxy_uri: Option<Uri>,
    /// Whether to check environment variables for proxy
    proxy_from_env: bool,
}

impl ProxyConnector {
    pub(crate) fn new(
        http: HttpConnector,
        proxy_uri: Option<Uri>,
        proxy_from_env: bool,
        _target_uri: &Uri,
    ) -> Self {
        Self {
            http,
            proxy_uri,
            proxy_from_env,
        }
    }

    /// Get the effective proxy URI for a given target URI
    fn get_effective_proxy_uri(&self, target_uri: &Uri) -> Option<Uri> {
        // Explicit proxy takes precedence
        if let Some(ref proxy_uri) = self.proxy_uri {
            return Some(proxy_uri.clone());
        }

        // Otherwise check environment if enabled
        if self.proxy_from_env {
            return get_proxy_from_env(target_uri);
        }

        None
    }
}

impl Service<Uri> for ProxyConnector {
    type Response = <HttpConnector as Service<Uri>>::Response;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.http
            .poll_ready(cx)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        // Determine if we need a proxy for this specific URI
        if let Some(proxy_uri) = self.get_effective_proxy_uri(&uri) {
            // Create a tunnel for this specific connection
            let mut tunnel = Tunnel::new(proxy_uri, self.http.clone());
            let fut = tunnel.call(uri);
            Box::pin(async move {
                fut.await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            })
        } else {
            // Direct connection
            let fut = self.http.call(uri);
            Box::pin(async move {
                fut.await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            })
        }
    }
}

/// Get proxy URI from environment variables for a given target URI.
fn get_proxy_from_env(target_uri: &Uri) -> Option<Uri> {
    let scheme = target_uri.scheme_str().unwrap_or("http");

    // Check no_proxy first
    if let Ok(no_proxy) = env::var("no_proxy").or_else(|_| env::var("NO_PROXY")) {
        if let Some(host) = target_uri.host() {
            for no_proxy_host in no_proxy.split(',') {
                let no_proxy_host = no_proxy_host.trim();
                if host == no_proxy_host || host.ends_with(&format!(".{no_proxy_host}")) {
                    return None;
                }
            }
        }
    }

    // Get proxy for the scheme
    let proxy_var = if scheme == "https" {
        env::var("https_proxy").or_else(|_| env::var("HTTPS_PROXY"))
    } else {
        env::var("http_proxy").or_else(|_| env::var("HTTP_PROXY"))
    };

    proxy_var.ok().and_then(|proxy_url| proxy_url.parse().ok())
}
