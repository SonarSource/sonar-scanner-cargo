#!/usr/bin/env bash
set -eo pipefail

# One `key = "value"` out of one [package.metadata.binstall...] section, empty if either
# the section or the key is absent.
toml_value() {
  awk -v section="[$1]" -v key="$2" '
    $0 == section { inside = 1; next }
    /^\[/ { inside = 0 }
    inside && index($0, key " = \"") == 1 {
      sub("^" key " = \"", ""); sub(/"$/, ""); print; exit
    }
  ' Cargo.toml
}
# The value binstall will use for a target: its override if it has one, else the default.
setting() {
  local override
  override=$(toml_value "package.metadata.binstall.overrides.$1" "$2")
  printf '%s' "${override:-$(toml_value package.metadata.binstall "$2")}"
}
# The subset of binstall's template variables these URLs use. `{ version }` is the crate
# version, without the build number — crates.io never sees one — and `{ build }` is
# gh-action_release's own placeholder, substituted when the crate is published.
render() {
  local out=$1 ext=""
  [[ "$2" == *windows* ]] && ext=".exe"
  out=${out//\{ name \}/cargo-sonar-scanner}
  out=${out//\{ bin \}/cargo-sonar-scanner}
  out=${out//\{ version \}/$RELEASE_VERSION}
  out=${out//\{ build \}/$BUILD_NUMBER}
  out=${out//\{ target \}/$2}
  out=${out//\{ binary-ext \}/$ext}
  printf '%s' "$out"
}
if [[ -z "$(toml_value package.metadata.binstall pkg-url)" ]]; then
  echo "::error::Cargo.toml has no [package.metadata.binstall] pkg-url"
  exit 1
fi
failed=0
while read -r target ext; do
  resolved=$(render "$(setting "$target" pkg-url)" "$target")
  # Anything left over is a variable this check does not know how to resolve, which means
  # it cannot vouch for the URL either way.
  if [[ "$resolved" == *'{'* || "$resolved" == *'}'* ]]; then
    echo "::error::unresolved placeholder in the $target pkg-url: $resolved"
    failed=1
    continue
  fi
  # Binaries.get_file_bucket_key: `Distribution/<artifactId>` for an org.sonarsource group
  # id, flat, with the filename gh-action_release rebuilds from ARTIFACTS_TO_PUBLISH.
  expected="https://binaries.sonarsource.com/Distribution/cargo-sonar-scanner/cargo-sonar-scanner-${PROJECT_VERSION}-${target}.${ext}"
  if [[ "$resolved" != "$expected" ]]; then
    echo "::error::binstall would fetch $target from"
    echo "::error::  $resolved"
    echo "::error::but the release publishes"
    echo "::error::  $expected"
    failed=1
    continue
  fi
  # `pkg-fmt` decides how binstall unpacks what it downloaded, independently of the URL, so
  # a `.zip` served to a `tgz` reader is a well-formed URL that fails at extraction.
  case "$ext" in
    tar.gz) want_fmt=tgz ;;
    zip)    want_fmt=zip ;;
    *) echo "::error::no pkg-fmt known for a .$ext archive"; failed=1; continue ;;
  esac
  fmt=$(setting "$target" pkg-fmt)
  if [[ "$fmt" != "$want_fmt" ]]; then
    echo "::error::$target ships a .$ext but its pkg-fmt is '${fmt:-unset}', expected '$want_fmt'"
    failed=1
    continue
  fi
  # `bin-dir` is where binstall looks *inside* the archive. Resolve it and check the entry
  # is really there, rather than trusting that the template still matches the layout the
  # binaries job stages — the archive is right here, so ask it.
  bin_dir=$(render "$(setting "$target" bin-dir)" "$target")
  archive="binaries/cargo-sonar-scanner-${PROJECT_VERSION}-${target}.${ext}"
  # Same fallback as the Pack (zip) step: a minimal runner has neither zip nor unzip.
  if [[ "$ext" == zip ]]; then
    if command -v unzip > /dev/null; then
      contents=$(unzip -Z1 "$archive")
    elif command -v python3 > /dev/null; then
      contents=$(python3 -c 'import sys,zipfile;print("\n".join(zipfile.ZipFile(sys.argv[1]).namelist()))' "$archive")
    else
      echo "::error::neither unzip nor python3 is available to list $archive"
      failed=1
      continue
    fi
  else
    contents=$(tar -tzf "$archive")
  fi
  if ! grep -qxF "$bin_dir" <<< "$contents"; then
    echo "::error::binstall would look for '$bin_dir' inside $archive, which contains:"
    while IFS= read -r entry; do echo "::error::  $entry"; done <<< "$contents"
    failed=1
    continue
  fi
  echo "ok  $target -> $resolved ($fmt, $bin_dir)"
done < "$RUNNER_TEMP/targets"
# The other direction. The loop above only visits targets the build produced, so an
# override left behind for a target dropped from the matrix is never resolved: binstall
# would send that platform to a URL nothing publishes and quietly fall back to compiling,
# which is the outcome the prebuilt binaries exist to avoid.
#
# Every override key is enumerated, quoted or not — TOML allows either `'` or `"` around a key
# that needs quoting, and binstall treats both the same. A quoted `cfg(...)` predicate is then
# skipped rather than compared: it names no single target, so it cannot be checked against the
# flat target list the way a bare triple can, and treating it as one would misreport it as an
# override for a target the binaries job does not build.
while read -r override; do
  [[ "$override" == 'cfg('* ]] && continue
  if ! cut -d' ' -f1 "$RUNNER_TEMP/targets" | grep -qxF "$override"; then
    echo "::error::Cargo.toml has a binstall override for $override, which the binaries job does not build"
    failed=1
  fi
done < <(sed -n 's/^\[package\.metadata\.binstall\.overrides\.\(.*\)\]$/\1/p' Cargo.toml \
           | sed -e "s/^['\"]//" -e "s/['\"]\$//")
exit "$failed"
