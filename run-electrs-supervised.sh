#!/bin/bash
# Supervise the Sequentia electrs: restart it whenever it exits.
#
# Sequentia reorganises whenever Bitcoin reorganises the block a Sequentia block
# is anchored to, at any depth, and electrs exits on some failures while
# following a reorganisation (a block that vanishes while it is being fetched,
# an RPC error from a node that is restarting). Restarting is all it takes: the
# index records which blocks it has fully indexed, so on start electrs walks
# back from the node's best block to one it holds, undoes the rows of every
# block that left the chain and indexes the new ones. The database is kept
# across every restart, so the API serves its last state at once.
#
# Override NODE_DIR/RPC_ADDR/DB_DIR/HTTP_ADDR/ELECTRUM_ADDR (and
# CONTRACT_REGISTRY_URL) via env; ELECTRS_BIN, NETWORK and ELECTRS_ARGS (extra
# flags, split on spaces) point it at another binary or chain.
set -u
cd "$(dirname "$0")/electrs"
[ -f ../env.sh ] && source ../env.sh

ELECTRS_BIN="${ELECTRS_BIN:-./target/debug/electrs}"
NETWORK="${NETWORK:-sequentiatest}"
read -r -a EXTRA_ARGS <<< "${ELECTRS_ARGS:-}"
NODE_DIR="${NODE_DIR:-$HOME/seq-testnet/node000}"
RPC_ADDR="${RPC_ADDR:-127.0.0.1:18200}"   # the public box's custom rpcport; a stock chain=test node serves 18776
HTTP_ADDR="${HTTP_ADDR:-127.0.0.1:3003}"
ELECTRUM_ADDR="${ELECTRUM_ADDR:-127.0.0.1:51402}"
DB_DIR="${DB_DIR:-/tmp/electrs-seq-db-sup}"
# RPC auth: set COOKIE=user:password only for a node configured with
# rpcuser/rpcpassword. Unset, electrs reads the node's own cookie file
# ($NODE_DIR/testnet3/.cookie), so no credential lives in this script.
COOKIE_ARGS=()
if [ -n "${COOKIE:-}" ]; then COOKIE_ARGS=(--cookie "$COOKIE"); fi
BACKOFF="${BACKOFF:-10}"
# The registry whose verified contract templates name contract spends
# (GET /tx/:txid/contracts and /contracts/...). Unset, spends are decoded
# unnamed.
REGISTRY_ARGS=()
if [ -n "${CONTRACT_REGISTRY_URL:-}" ]; then REGISTRY_ARGS=(--contract-registry-url "$CONTRACT_REGISTRY_URL"); fi

mkdir -p "$DB_DIR"
n=0
while true; do
  n=$((n+1))
  echo "[supervisor] start #$n -> $RPC_ADDR (db $DB_DIR)  $(date -u +%H:%M:%S)"
  "$ELECTRS_BIN" \
    --network "$NETWORK" \
    --daemon-rpc-addr "$RPC_ADDR" \
    --daemon-dir "$NODE_DIR" \
    ${COOKIE_ARGS[@]+"${COOKIE_ARGS[@]}"} \
    ${REGISTRY_ARGS[@]+"${REGISTRY_ARGS[@]}"} \
    --db-dir "$DB_DIR" \
    --http-addr "$HTTP_ADDR" \
    --electrum-rpc-addr "$ELECTRUM_ADDR" \
    --cors '*' \
    ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"} \
    --jsonrpc-import -vv
  rc=$?
  echo "[supervisor] electrs exited (rc=$rc); its index in $DB_DIR is kept"
  echo "[supervisor] restarting in ${BACKOFF}s..."
  sleep "$BACKOFF"
done
