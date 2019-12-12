resource "tls_private_key" "server" {
  algorithm = "RSA"
  rsa_bits = "2048"
}

resource "tls_cert_request" "server" {
  key_algorithm = tls_private_key.server.algorithm
  private_key_pem = tls_private_key.server.private_key_pem

  subject {
    common_name = "Tonic Test Server Cert"
  }

  dns_names = [
    "*.test.google.fr",
  ]
}

resource "tls_locally_signed_cert" "server" {
  cert_request_pem = tls_cert_request.server.cert_request_pem

  ca_key_algorithm = tls_private_key.root.algorithm
  ca_private_key_pem = tls_private_key.root.private_key_pem
  ca_cert_pem = tls_self_signed_cert.root.cert_pem

  validity_period_hours = 43800
  early_renewal_hours = 8760

  allowed_uses = ["server_auth"]
}

resource "local_file" "server_cert" {
  filename = "../server1.pem"
  content = tls_locally_signed_cert.server.cert_pem
}

resource "local_file" "server_key" {
  filename = "../server1.key"
  content = tls_private_key.server.private_key_pem
}
