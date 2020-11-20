use crate::load_balanced_channel::{
    DnsResolver, GrpcServiceProbe, GrpcServiceProbeConfig, LookupService, ServiceDefinition,
};
use http::Request;
use std::task::{Context, Poll};
use tokio::time::Duration;
use tonic::body::BoxBody;
use tonic::client::GrpcService;
use tonic::transport::channel::{Channel, ClientTlsConfig};

// Determines the channel size of the channel we use
// to report endpoint changes to tonic.
// This is effectively how many changes we can report in one go.
// We set the number high to avoid any blocking on our side.
static GRPC_REPORT_ENDPOINTS_CHANNEL_SIZE: usize = 1024;

/// Implements tonic `GrpcService` for a client-side load balanced `Channel` (using `The Power of
#[derive(Debug, Clone)]
pub struct LoadBalancedChannel(Channel);

impl Into<Channel> for LoadBalancedChannel {
    fn into(self) -> Channel {
        self.0
    }
}

impl GrpcService<BoxBody> for LoadBalancedChannel {
    type ResponseBody = <Channel as GrpcService<BoxBody>>::ResponseBody;
    type Error = <Channel as GrpcService<BoxBody>>::Error;
    type Future = <Channel as GrpcService<BoxBody>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: Request<BoxBody>) -> Self::Future {
        self.0.call(request)
    }
}

/// Builder to configure a `LoadBalancedChannel`.
pub struct LoadBalancedChannelBuilder<T> {
    service_definition: ServiceDefinition,
    probe_interval: Option<Duration>,
    timeout: Option<Duration>,
    tls_config: Option<ClientTlsConfig>,
    lookup_host: T,
}

impl LoadBalancedChannelBuilder<DnsResolver> {
    /// Set the `ServiceDefinition` of the gRPC server service
    /// -  e.g. `google.com` and `5000`.
    ///
    /// All the service endpoints of a `ServiceDefinition` will be
    /// constructed by resolving all ips from `ServiceDefinition::hostname`, and
    /// using the portnumber `ServiceDefinition::port`.
    pub async fn new_with_service<H: Into<ServiceDefinition>>(
        service_definition: H,
    ) -> Result<LoadBalancedChannelBuilder<DnsResolver>, anyhow::Error> {
        Ok(Self {
            service_definition: service_definition.into(),
            probe_interval: None,
            timeout: None,
            tls_config: None,
            lookup_host: DnsResolver::from_system_config().await?,
        })
    }

    /// Configure the channel to use tls.
    /// A `tls_config` MUST be specified to use the `HTTPS` scheme.
    pub fn lookup_host<T: LookupService + Send + Sync + 'static>(
        self,
        lookup_host: T,
    ) -> LoadBalancedChannelBuilder<T> {
        LoadBalancedChannelBuilder {
            lookup_host,
            service_definition: self.service_definition,
            probe_interval: self.probe_interval,
            tls_config: self.tls_config,
            timeout: self.timeout,
        }
    }
}

impl<T: LookupService + Send + Sync + 'static + Sized> LoadBalancedChannelBuilder<T> {
    /// Set the how often, the client should probe for changes to  gRPC server endpoints.
    /// Default interval in seconds is 10.
    pub fn dns_probe_interval(self, interval: Duration) -> LoadBalancedChannelBuilder<T> {
        Self {
            probe_interval: Some(interval),
            ..self
        }
    }

    /// Set a timeout that will be applied to every new `Endpoint`.
    pub fn timeout(self, timeout: Duration) -> LoadBalancedChannelBuilder<T> {
        Self {
            timeout: Some(timeout),
            ..self
        }
    }

    /// Configure the channel to use tls.
    /// A `tls_config` MUST be specified to use the `HTTPS` scheme.
    pub fn with_tls(self, mut tls_config: ClientTlsConfig) -> LoadBalancedChannelBuilder<T> {
        // Since we resolve the hostname to an IP, which is not a valid DNS name,
        // we have to set the hostname explicitly on the tls config,
        // otherwise the IP will be set as the domain name and tls handshake will fail.
        tls_config = tls_config.domain_name(self.service_definition.hostname.clone());

        Self {
            tls_config: Some(tls_config),
            ..self
        }
    }

    /// Construct a `LoadBalancedChannel` from the `LoadBalancedChannelBuilder` instance.
    pub fn channel(self) -> LoadBalancedChannel {
        let (channel, sender) = Channel::balance_channel(GRPC_REPORT_ENDPOINTS_CHANNEL_SIZE);

        let config = GrpcServiceProbeConfig {
            service_definition: self.service_definition,
            dns_lookup: self.lookup_host,
            endpoint_timeout: self.timeout,
            probe_interval: self
                .probe_interval
                .unwrap_or_else(|| Duration::from_secs(10)),
        };
        let mut service_probe = GrpcServiceProbe::new_with_reporter(config, sender);

        if let Some(tls_config) = self.tls_config {
            service_probe = service_probe.with_tls(tls_config);
        }

        tokio::spawn(service_probe.probe());

        LoadBalancedChannel(channel)
    }
}
