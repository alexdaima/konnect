# Konnect

Konnect is a (currently macOS only) toolbar application for Kubernetes where it handles concurrent port-forwarding for multiple clusters and exposes them as useful local DNS names like `us-west-2.hyperdx.localhost:1355`.

## Install

```sh
cargo install --path .
konnect init  # Writes the config.example.json to ~/.konnect/config.json
konnect  # Start Konnect
```

## Configuration

Konnect picks its configuration file in this order:

1. `--config <PATH>` (short: `-c`)
2. `$KONNECT_CONFIG`
3. `~/.konnect/config.json`

To keep the routes for a project in the repository itself, so everyone working on
it shares one definition:

```sh
konnect --config ./konnect.json init  # Writes konnect.json and its JSON schema
konnect --config ./konnect.json list
```

`--config` is global, so it works before or after the subcommand
(`konnect list -c ./konnect.json`). Paths are resolved against the current
directory, so prefer an absolute path when Konnect may be run from a
subdirectory — with direnv, `export KONNECT_CONFIG="$PWD/konnect.json"`, and with
Devbox, `"env": {"KONNECT_CONFIG": "$DEVBOX_PROJECT_ROOT/konnect.json"}`.
