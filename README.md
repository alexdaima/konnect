# Konnect

Konnect is a (currently macOS only) toolbar application for Kubernetes where it handles concurrent port-forwarding for multiple clusters and exposes them as useful local DNS names like `us-west-2.hyperdx.localhost:1355`.

## Install

```sh
cargo install --path .
konnect init  # Writes the config.example.json to ~/.konnect/config.json
konnect  # Start Konnect
```

## Raw TCP routes

The named `<cluster>.<service>.localhost` routes all share one proxy port, so
Konnect has to read the destination out of each connection to know where to send
it. Only protocols that carry their destination can be routed this way — in
practice, HTTP and its `Host` header. The PostgreSQL wire protocol, MySQL, Redis
and plain TCP never send one, so those connections cannot use a shared port.

Give such a service its own port with `local_port`:

```json
{
  "name": "usa-postgres",
  "namespace": "namespace",
  "target": "svc/postgres",
  "remote_port": 5432,
  "local_port": 15432
}
```

Konnect then listens on `127.0.0.1:15432` and relays it byte for byte, with
nothing parsed. Point your client (TablePlus, `psql`, anything) straight at that
port.

`local_port` belongs on a specific cluster, not on `clusters.all` — every cluster
would otherwise try to bind the same port. Give the same service a different port
in each cluster, which is also what keeps `usa` and `china` distinguishable when
the protocol offers no hostname to tell them apart.

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
