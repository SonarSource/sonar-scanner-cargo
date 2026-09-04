#!/usr/bin/env bash
set -eo pipefail

jf config remove repox > /dev/null 2>&1 || true
jf config add repox \
  --url https://repox.jfrog.io \
  --artifactory-url https://repox.jfrog.io/artifactory \
  --access-token "$ARTIFACTORY_DEPLOY_TOKEN"
jf config use repox
crate="cargo-sonar-scanner-${PROJECT_VERSION}.crate"
dir="crates/cargo-sonar-scanner"
# A synthetic Maven coordinate, because releasability insists on one: ArtifactoryId.create
# parses every build-info module id as groupId:artifactId:version and throws on anything
# else. Without --module the id defaults to the build name, so CheckManifestValues fails
# with "sonar-scanner-cargo could not be parsed" before it looks at anything. Having a
# parseable coordinate at all is what fixes that; the check then passes because
# checkManifestsOfCommercialPlugins keeps only modules that are all of com.sonarsource.*,
# named sonar-*-plugin, and carrying a plugin JAR, and this is none of the three.
# Same pattern as sonarqube-cli, the other multi-platform CLI with no Maven build.
#
# org.sonarsource.* rather than com.sonarsource.* is a separate call: this is a public
# repository, and gh-action_release routes binaries by group id — get_binaries_repo() in
# main/release/utils/binaries.py sends com.* to CommercialDistribution and everything else
# to the public Distribution path.
module="org.sonarsource.scanner.cargo:cargo-sonar-scanner:${PROJECT_VERSION}"
# --flat, or the source hierarchy is preserved and the crate lands under a `target/package`
# of its own. --fail-no-op, or an empty build info would silently promote nothing.
jf rt upload --flat=true --fail-no-op \
  --build-name="$BUILD_NAME" --build-number="$BUILD_NUMBER" --module="$module" \
  "target/package/$crate" "${ARTIFACTORY_DEPLOY_REPO}/${dir}/"
# A crate anywhere but `crates/<name>/<name>-<version>.crate` is stored but never indexed,
# so it would be published and still unresolvable.
if ! jf rt search "${ARTIFACTORY_DEPLOY_REPO}/${dir}/${crate}" | grep -q "$crate"; then
  echo "::error::${dir}/${crate} is not in ${ARTIFACTORY_DEPLOY_REPO}"
  exit 1
fi
# Same build info and the same module as the crate, so a release promotes the binaries with
# it rather than leaving them behind in `-qa`. An upload without --module would register a
# second module named after the build, and releasability would throw on that one instead.
#
# Maven layout, not because anything indexes it, but because it is the only shape the
# release can fetch from: Artifactory.download builds
# `<repo>/<groupId as path>/<artifactId>/<version>/<artifactId>-<version>-<qual>.<ext>`
# out of each ARTIFACTS_TO_PUBLISH entry, and 404s on anything else.
jf rt upload --flat=true --fail-no-op \
  --build-name="$BUILD_NAME" --build-number="$BUILD_NUMBER" --module="$module" \
  "binaries/*" \
  "${ARTIFACTORY_DEPLOY_REPO}/org/sonarsource/scanner/cargo/cargo-sonar-scanner/${PROJECT_VERSION}/"
jf rt build-collect-env "$BUILD_NAME" "$BUILD_NUMBER"
jf rt build-add-git "$BUILD_NAME" "$BUILD_NUMBER"
jf rt build-publish "$BUILD_NAME" "$BUILD_NUMBER"
