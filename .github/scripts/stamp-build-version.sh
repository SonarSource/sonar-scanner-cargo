#!/usr/bin/env bash
set -eo pipefail

release=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
if [[ -z "$release" ]]; then
  echo "::error::Cargo.toml declares no version"
  exit 1
fi
version="${release}-${BUILD_NUMBER}"
# The package's own version only: the first in Cargo.toml, the line after its name in
# Cargo.lock. Dependency versions are left be.
awk -v v="$version" '!done && /^version = /{print "version = \"" v "\""; done=1; next} {print}' \
  Cargo.toml > Cargo.toml.stamped && mv Cargo.toml.stamped Cargo.toml
awk -v v="$version" '/^name = "cargo-sonar-scanner"$/{print; getline; print "version = \"" v "\""; next} {print}' \
  Cargo.lock > Cargo.lock.stamped && mv Cargo.lock.stamped Cargo.lock
# Reaches the build info via `jf rt build-collect-env` below, where the promotion and the
# release read it back.
echo "PROJECT_VERSION=$version" >> "$GITHUB_ENV"
# The build number stripped back off, which is what crates.io publishes and therefore what
# `cargo binstall` templates as `{ version }`. Only the binstall check below needs it.
echo "RELEASE_VERSION=$release" >> "$GITHUB_ENV"
echo "Building $BUILD_NAME/$BUILD_NUMBER as $version"
