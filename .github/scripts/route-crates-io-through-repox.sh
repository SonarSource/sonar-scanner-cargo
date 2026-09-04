#!/usr/bin/env bash
set -eo pipefail

# Dependencies resolve through Repox rather than crates.io directly. This file is generated
# here and never committed: committing it would break external contributors, who have no
# Repox credentials.
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[registry]
# Repox requires authentication, and Cargo refuses to talk to an authenticated registry
# without an explicit credential provider. The token comes from the environment.
global-credential-providers = ["cargo:token"]

[registries.repox]
index = "sparse+https://repox.jfrog.io/artifactory/api/cargo/crates-io/index/"

[source.crates-io]
replace-with = "repox"
EOF
