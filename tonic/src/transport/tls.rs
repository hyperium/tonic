#[derive(Debug, Clone)]
pub struct Certificate {
    pub(crate) pem: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub(crate) cert: Certificate,
    pub(crate) key: Vec<u8>,
}

impl Certificate {
    pub fn from_pem(pem: Vec<u8>) -> Self {
        Self { pem }
    }
}

impl Identity {
    pub fn from_pem(cert: Vec<u8>, key: Vec<u8>) -> Self {
        let cert = Certificate::from_pem(cert);
        Self { cert, key }
    }
}
