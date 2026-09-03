#!/usr/bin/env bash
set -eo pipefail

# A throwaway keyring, so the key never touches the runner's default GNUPGHOME.
GNUPGHOME=$(mktemp -d)
export GNUPGHOME
chmod 700 "$GNUPGHOME"
printf '%s\n' "$GPG_SIGNING_KEY" | gpg --batch --quiet --import
cd binaries
# Expanded once, before the loop body creates any `.asc`, so signatures are not signed.
for file in cargo-sonar-scanner-*; do
  printf '%s' "$GPG_SIGNING_PASSPHRASE" | gpg --batch --quiet \
    --pinentry-mode loopback --passphrase-fd 0 --detach-sign --armor "$file"
  gpg --batch --quiet --verify "$file.asc" "$file"
  echo "signed $file"
done
