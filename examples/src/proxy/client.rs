/// Example: Using Tonic with HTTP Proxy Support
/// 
/// This example demonstrates how to use the proxy functionality in Tonic.
/// The proxy support includes both explicit proxy configuration and automatic
/// detection from environment variables.

use tonic::transport::Endpoint;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Example 1: Explicit proxy configuration
    let endpoint_with_proxy = Endpoint::from_static("https://httpbin.org/get")
        .proxy_uri("http://username:password@proxy.example.com:8080".parse()?);

    // Example 2: Environment-based proxy detection
    let endpoint_with_env_proxy = Endpoint::from_static("https://api.github.com")
        .proxy_from_env(true);

    // Example 3: Both explicit proxy and environment detection
    let endpoint_combined = Endpoint::from_static("http://example.com")
        .proxy_uri("http://explicit-proxy.com:3128".parse()?)
        .proxy_from_env(true);

    // Example 4: Creating a lazy channel (doesn't actually connect)
    let _channel = endpoint_with_proxy.connect_lazy();

    Ok(())
}
