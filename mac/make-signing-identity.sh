#!/bin/bash
# One-time setup on the Mac that builds Fruity Bridge: creates a self-signed code-signing identity
# in its own keychain and adds that keychain to the user's search list so codesign can use it.
# A stable identity means macOS keeps the app's permissions across rebuilds.
set -euo pipefail
dir="$HOME/Library/Application Support/Fruity Bridge/signing"
keychain="$dir/signing.keychain-db"
name="Fruity Bridge Local Signing"
if [[ -f "$keychain" ]]; then
  echo "Signing keychain already exists: $keychain"
else
  mkdir -p "$dir" && chmod 700 "$dir"
  work="$(mktemp -d)" && trap 'rm -rf "$work"' EXIT
  password="$(openssl rand -hex 24)"
  printf %s "$password" > "$dir/keychain-password" && chmod 600 "$dir/keychain-password"
  cat > "$work/cert.cnf" <<CNF
[req]
distinguished_name=dn
x509_extensions=ext
prompt=no
[dn]
CN=$name
[ext]
basicConstraints=critical,CA:false
keyUsage=critical,digitalSignature
extendedKeyUsage=critical,codeSigning
CNF
  openssl req -x509 -newkey rsa:2048 -nodes -days 7300 -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/cert.cnf" 2>/dev/null
  p12_password="$(openssl rand -hex 12)"
  openssl pkcs12 -export -legacy -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/id.p12" -passout "pass:$p12_password" 2>/dev/null \
    || openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" -out "$work/id.p12" -passout "pass:$p12_password"
  security create-keychain -p "$password" "$keychain"
  security set-keychain-settings "$keychain"
  security unlock-keychain -p "$password" "$keychain"
  security import "$work/id.p12" -k "$keychain" -P "$p12_password" -T /usr/bin/codesign
  security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
fi
# Keep existing keychains and append ours if missing.
current=()
while IFS= read -r line; do
  line="${line#"${line%%[![:space:]]*}"}"; line="${line%\"}"; line="${line#\"}"
  [[ -n "$line" ]] && current+=("$line")
done < <(security list-keychains -d user)
if ! printf '%s\n' "${current[@]}" | grep -Fxq "$keychain"; then
  security list-keychains -d user -s "${current[@]}" "$keychain"
fi
echo "Signing identity ready: $name"
