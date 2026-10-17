#!/usr/bin/env bash
# ACME deploy hook: publish a complete pair, then notify both TLS listeners.
set -euo pipefail
if [[ $# != 2 ]]; then
    echo "Usage: $0 SERVER_NAME RENEWED_LINEAGE" >&2
    exit 2
fi
server_name=$1
lineage=$2
root=${FOLDFIN_TLS_DIR:-/etc/floppa-foldfin/tls}
service=${FOLDFIN_SERVICE:-floppa-foldfin.service}
# Validity, SAN and matching key are checked before changing the live pointer.
openssl x509 -in "$lineage/fullchain.pem" -noout -checkend 300 >/dev/null
openssl x509 -in "$lineage/fullchain.pem" -noout -checkhost "$server_name" >/dev/null
certificate_key=$(openssl x509 -in "$lineage/fullchain.pem" -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256)
private_key=$(openssl pkey -in "$lineage/privkey.pem" -pubout -outform DER | openssl dgst -sha256)
[[ $certificate_key == "$private_key" ]] || { echo 'TLS certificate/key mismatch' >&2; exit 1; }
install -d -m 0750 -o root -g floppa-foldfin "$root"
candidate=$(mktemp -d "$root/identity.XXXXXXXX")
chown root:floppa-foldfin "$candidate"
chmod 0750 "$candidate"
install -m 0640 -o root -g floppa-foldfin "$lineage/fullchain.pem" "$candidate/fullchain.pem"
install -m 0640 -o root -g floppa-foldfin "$lineage/privkey.pem" "$candidate/privkey.pem"
# A same-filesystem rename publishes the directory pointer atomically.
ln -s "$(basename "$candidate")" "$candidate.next"
mv -Tf "$candidate.next" "$root/current"
if systemctl is-active --quiet "$service"; then
    systemctl reload "$service"
fi
# Keep previous identities for explicit rollback; never print private material.
