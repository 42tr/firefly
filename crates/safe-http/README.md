# Firefly Safe HTTP

Bounded HTTP requests for model-facing tools. Requests accept only HTTP(S),
never follow redirects, reject private and reserved IPs when no host allowlist
is supplied, and cap response data returned to the caller. An explicit
allowlist may opt into private hosts.

```sh
cargo test --locked -p firefly-safe-http
```
