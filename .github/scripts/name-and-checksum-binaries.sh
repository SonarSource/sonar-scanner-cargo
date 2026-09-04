#!/usr/bin/env bash
set -eo pipefail

cd binaries
# Every target the binaries job produced, as `<target> <ext>` pairs. The steps below derive
# what they publish from this rather than restating the matrix, so a target added there
# cannot be silently left out of a release.
: > "$RUNNER_TEMP/targets"
for archive in *; do
  case "$archive" in
    cargo-sonar-scanner-*.tar.gz) target="${archive#cargo-sonar-scanner-}"; target="${target%.tar.gz}"; ext="tar.gz" ;;
    cargo-sonar-scanner-*.zip)    target="${archive#cargo-sonar-scanner-}"; target="${target%.zip}";    ext="zip" ;;
    *) echo "::error::unexpected artefact $archive"; exit 1 ;;
  esac
  mv "$archive" "cargo-sonar-scanner-${PROJECT_VERSION}-${target}.${ext}"
  echo "$target $ext" >> "$RUNNER_TEMP/targets"
done
# Written outside the directory first: redirecting into it would have the shell create the
# file before the glob expands, and the checksum list would contain itself.
sha256sum cargo-sonar-scanner-* > ../checksums
# Deliberately not `.sha256`: Artifactory reads an uploaded `<file>.sha256` as a checksum
# to apply to `<file>`, and rejects the upload because no such artefact exists.
mv ../checksums "cargo-sonar-scanner-${PROJECT_VERSION}-checksums.txt"
ls -l
# What the release publishes to binaries.sonarsource.com, as `groupId:artifactId:ext:qual`.
# gh-action_release reads it back off the build info as `buildInfo.env.ARTIFACTS_TO_PUBLISH`
# (BuildInfo.get_artifacts_to_publish) and rebuilds each filename from the four parts, which
# is why the naming above has to be `<artifactId>-<version>-<qual>.<ext>` exactly.
artifacts=()
while read -r target ext; do
  artifacts+=("org.sonarsource.scanner.cargo:cargo-sonar-scanner:${ext}:${target}")
done < "$RUNNER_TEMP/targets"
printf 'ARTIFACTS_TO_PUBLISH=%s\n' "$(IFS=,; echo "${artifacts[*]}")" >> "$GITHUB_ENV"
