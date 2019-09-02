use super::tls::Cert;
use http::uri::Uri;

#[derive(Debug, Clone)]
pub struct Endpoint {
    uri: Uri,
    cert: Option<Cert>,
}

impl Endpoint {
    pub fn with_pem(uri: Uri, ca: Vec<u8>, domain: Option<String>) -> Self {
        let domain = domain.unwrap_or_else(|| uri.clone().to_string());

        Self {
            uri,
            cert: Some(Cert { ca, domain }),
        }
    }

    pub(crate) fn uri(&self) -> &Uri {
        &self.uri
    }

    pub(crate) fn take_cert(&mut self) -> Option<Cert> {
        self.cert.take()
    }
}

impl From<Uri> for Endpoint {
    fn from(uri: Uri) -> Self {
        Self { uri, cert: None }
    }
}
