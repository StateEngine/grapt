| Component | Source | License | Adaptation |
| --- | --- | --- | --- |
| Vertex service-account JWT signing and exchange | `dermesser/yup-oauth2`, `src/service_account.rs`, commit `0bbe68c95b63b91a68b60f9ea176c0de29f5002a` | MIT OR Apache-2.0; unchanged texts in `licenses/yup-oauth2/` | Rustls RS256 signer and JWT bearer exchange, modified for fixed Google issuer, injected clock, bounded responses, zeroizing secrets, and existing connection custody. Copyright (c) 2016 Google Inc (lewinb@google.com). |
