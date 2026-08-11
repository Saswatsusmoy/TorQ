#!/bin/sh
# Publish the workspace crates to crates.io in dependency order.
# Requires CARGO_REGISTRY_TOKEN (set it on the CI secret or locally).
set -eu

for crate in torq-sources torq-core torq-tui torqtui; do
    echo "publishing $crate"
    cargo publish -p "$crate"
done
