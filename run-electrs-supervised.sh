#!/bin/bash
# Supervise electrs against the (volatile) shared Sequentia testnet.
#
# Blockstream's electrs hard-panics if a block it is fetching gets reorged away
# (daemon.rs: "Block not found"). The shared testnet is being repeatedly reset /
# reorged by another process, so electrs dies each time. This wrapper restarts it
# with a fresh DB and a short backoff, so the explorer self-heals once the chain
# settles. Override NODE/RPC/DB/HTTP via env.
set -u
cd "$(dirname "$0")/electrs"
source ../env.sh

NODE_DIR="${NODE_DIR:-/home/aejkohl/seq-testnet/node000}"
RPC_ADDR="${RPC_ADDR:-127.0.0.1:18200}"
HTTP_ADDR="${HTTP_ADDR:-127.0.0.1:3003}"
ELECTRUM_ADDR="${ELECTRUM_ADDR:-127.0.0.1:51402}"
DB_DIR="${DB_DIR:-/tmp/electrs-seq-db-sup}"
COOKIE="${COOKIE:-seq:seq}"
BACKOFF="${BACKOFF:-10}"

mkdir -p "$DB_DIR"
n=0
while true; do
  n=$((n+1))
  echo "[supervisor] start #$n -> $RPC_ADDR (db $DB_DIR)  $(date -u +%H:%M:%S)"
  ./target/debug/electrs \
    --network sequentiatest \
    --daemon-rpc-addr "$RPC_ADDR" \
    --daemon-dir "$NODE_DIR" \
    --cookie "$COOKIE" \
    --db-dir "$DB_DIR" \
    --http-addr "$HTTP_ADDR" \
    --electrum-rpc-addr "$ELECTRUM_ADDR" \
    --cors '*' \
    --jsonrpc-import -vv
  rc=$?
  # The DB is REUSED across restarts so electrs serves its last-indexed state
  # immediately (even while the node is briefly down). A non-zero exit means a
  # panic — typically a reorg/wipe of the underlying chain left stale index
  # state, so drop the DB and re-index cleanly on the next start.
  if [ "$rc" -ne 0 ]; then
    echo "[supervisor] electrs crashed (rc=$rc); wiping DB to recover from a chain reorg/reset"
    rm -rf "$DB_DIR"; mkdir -p "$DB_DIR"
  else
    echo "[supervisor] electrs exited cleanly (rc=0)"
  fi
  echo "[supervisor] restarting in ${BACKOFF}s..."
  sleep "$BACKOFF"
done
