#!/bin/sh
# Boot a CKB dev-chain node (or its dummy miner) inside docker compose.
#
# First boot: `ckb init --chain dev` generates the canonical dev-chain config
# and chain spec into CKB_HOME. We then patch it so that:
#   - the JSON-RPC server binds on 0.0.0.0 (default is already 0.0.0.0 for
#     dev chain, but we defend against future template changes),
#   - the miner reaches the node via the compose service DNS name, and
#   - a `block_assembler` is configured so the dummy miner can request block
#     templates (a fresh `ckb init` leaves this commented out).
#
# Subsequent boots skip init and go straight to `ckb run` / `ckb miner`.

set -eu

CKB_HOME="${CKB_HOME:-/var/lib/ckb}"
mkdir -p "${CKB_HOME}"
cd "${CKB_HOME}"

if [ ! -f "${CKB_HOME}/ckb.toml" ]; then
    ckb init --chain dev --force

    # Bind RPC on every interface (dev chain already does this, but keep the
    # sed idempotent for safety).
    sed -i \
        -e 's|listen_address = "127.0.0.1:8114"|listen_address = "0.0.0.0:8114"|' \
        ckb.toml

    # Append a block_assembler so the dummy miner can build templates. The
    # args are a throwaway 20-byte value; this is a dev chain only.
    cat >> ckb.toml <<'EOF'

# Added by cellora entrypoint to enable dummy mining on the dev chain.
[block_assembler]
code_hash = "0x9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8"
args = "0x829f662faf88a2a2fa3f13d54d11a05dd0eb87d7"
hash_type = "type"
message = "0x"
use_binary_version_as_message_prefix = true
EOF

    # Point the miner at the `ckb` compose service and shorten the block
    # cadence so the indexer has a constant stream to ingest.
    sed -i \
        -e 's|rpc_url = "http://0.0.0.0:8114/"|rpc_url = "http://cellora-ckb:8114/"|' \
        -e 's|rpc_url = "http://127.0.0.1:8114/"|rpc_url = "http://cellora-ckb:8114/"|' \
        -e 's|^value = 5000$|value = 1000|' \
        ckb-miner.toml
fi

cmd="${1:-run}"
shift || true

case "${cmd}" in
    run)
        exec ckb run -C "${CKB_HOME}" "$@"
        ;;
    miner)
        exec ckb miner -C "${CKB_HOME}" "$@"
        ;;
    *)
        exec ckb -C "${CKB_HOME}" "${cmd}" "$@"
        ;;
esac
