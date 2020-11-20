/// Defines a gRPC service with a `hostname` and a `port`.
/// The hostname will be resolved to the concrete ips of the service servers.
#[derive(Debug)]
pub struct ServiceDefinition {
    /// The hostname of the service.
    pub hostname: String,
    /// The service port.
    pub port: u16,
}

impl Into<ServiceDefinition> for (&str, u16) {
    fn into(self) -> ServiceDefinition {
        ServiceDefinition {
            hostname: self.0.to_string(),
            port: self.1,
        }
    }
}
