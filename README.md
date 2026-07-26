# Konnect

Konnect is a (currently macOS only) toolbar application for Kubernetes where it handles concurrent port-forwarding for multiple clusters and exposes them as useful local DNS names like `us-west-2.hyperdx.localhost:1355`.

## Install

```sh
cargo install --path .
konnect init  # Writes the config.example.json to ~/.konnect/config.json
konnect  # Start Konnect
```
