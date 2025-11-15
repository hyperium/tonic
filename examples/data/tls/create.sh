#!/bin/bash

# Create the server CA certs.
openssl req -x509                                     \
  -newkey rsa:4096                                    \
  -nodes                                              \
  -days 3650                                          \
  -keyout ca.key                           \
  -out ca.pem                             \
  -subj /O=Tonic/CN=test-server_ca/   \
  -config ./openssl.cnf                               \
  -extensions test_ca                                 \
  -sha256

# Create the client CA certs.
openssl req -x509                                     \
  -newkey rsa:4096                                    \
  -nodes                                              \
  -days 3650                                          \
  -keyout client_ca.key                           \
  -out client_ca.pem                             \
  -subj /O=Tonic/CN=test-client_ca/   \
  -config ./openssl.cnf                               \
  -extensions test_ca                                 \
  -sha256

# Generate one server cert.
openssl genrsa -out server.key 4096
openssl req -new                                    \
  -key server.key                              \
  -out server_csr.pem                              \
  -subj /O=Tonic/CN=test-server/   \
  -config ./openssl.cnf                             \
  -reqexts test_server
openssl x509 -req           \
  -in server_csr.pem       \
  -CAkey ca.key  \
  -CA ca.pem    \
  -days 3650                \
  -set_serial 1000          \
  -out server.pem     \
  -extfile ./openssl.cnf    \
  -extensions test_server   \
  -sha256
openssl verify -verbose -CAfile ca.pem  server.pem

# Generate two client certs.
openssl genrsa -out client1.key 4096
openssl req -new                                    \
  -key client1.key                              \
  -out client1_csr.pem                              \
  -subj /O=Tonic/CN=test-client1/   \
  -config ./openssl.cnf                             \
  -reqexts test_client
openssl x509 -req           \
  -in client1_csr.pem       \
  -CAkey client_ca.key  \
  -CA client_ca.pem    \
  -days 3650                \
  -set_serial 1000          \
  -out client1.pem     \
  -extfile ./openssl.cnf    \
  -extensions test_client   \
  -sha256
openssl verify -verbose -CAfile client_ca.pem  client1.pem

openssl genrsa -out client2.key 4096
openssl req -new                                    \
  -key client2.key                              \
  -out client2_csr.pem                              \
  -subj /O=Tonic/CN=test-client2/   \
  -config ./openssl.cnf                             \
  -reqexts test_client
openssl x509 -req           \
  -in client2_csr.pem       \
  -CAkey client_ca.key  \
  -CA client_ca.pem    \
  -days 3650                \
  -set_serial 1000          \
  -out client2.pem     \
  -extfile ./openssl.cnf    \
  -extensions test_client   \
  -sha256
openssl verify -verbose -CAfile client_ca.pem  client2.pem

# Cleanup the CSRs.
rm *_csr.pem
