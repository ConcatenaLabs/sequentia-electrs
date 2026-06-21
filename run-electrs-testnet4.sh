#!/bin/bash
# Bitcoin testnet4 electrs (the parent-chain indexer), pointed at the local
# bitcoind (:48332). Stable chain (no churn), so systemd Restart=always is enough
# — no supervisor needed. Binary + DB live under a persistent path so they
# survive a reboot (unlike /tmp).
set -e
PW=$(grep -E "^rpcpassword=" "$HOME/.bitcoin/bitcoin.conf" | head -1 | cut -d= -f2)
BIN="${ELECTRS_BTC_BIN:-$HOME/.local/share/concatena-explorer/bin/electrs-bitcoin}"
DB="${T4_DB:-$HOME/.local/share/concatena-explorer/t4-db}"
mkdir -p "$DB"
exec "$BIN" \
  --network testnet4 \
  --daemon-rpc-addr 127.0.0.1:48332 \
  --daemon-dir "$HOME/.bitcoin" \
  --cookie "seq:$PW" \
  --db-dir "$DB" \
  --http-addr 127.0.0.1:3004 \
  --electrum-rpc-addr 127.0.0.1:51403 \
  --monitoring-addr 127.0.0.1:44425 \
  --cors '*' --jsonrpc-import -vv
