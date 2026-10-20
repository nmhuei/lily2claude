set -eu

CONFIG=/etc/sing-box/config.json
TMP=$CONFIG.tmp
BAK=$CONFIG.bak
WARP_API="${WARP_API:-https://api.cloudflareclient.com/v0a2025/reg}"
TOS="$(date -u +%Y-%m-%dT%H:%M:%S).000Z"

SECRET_KEY=$(openssl genpkey -algorithm X25519 2>/dev/null)
CF_PRIVATE_KEY=$(printf "%s\n" "$SECRET_KEY" | openssl pkey -outform DER 2>/dev/null | tail -c 32 | base64)
CF_LOCAL_KEY=$(printf "%s\n" "$SECRET_KEY" | openssl pkey -pubout -outform DER 2>/dev/null | tail -c 32 | base64)
WARP_PAYLOAD=$(jq -n --arg tos "$TOS" --arg key "$CF_LOCAL_KEY" '{"tos": $tos, "key": $key}')

RESPONSE=$(curl -fsSL -m 8 -X POST "$WARP_API" -H "Content-Type: application/json" -d "$WARP_PAYLOAD")
CF_CLIENT_ID=$(printf "%s\n" "$RESPONSE" | jq -r ".config.client_id // empty")
CF_PUBLIC_KEY=$(printf "%s\n" "$RESPONSE" | jq -r ".config.peers[0].public_key // empty")
CF_ADDR_V4=$(printf "%s\n" "$RESPONSE" | jq -r ".config.interface.addresses.v4 // empty")
CF_ADDR_V6=$(printf "%s\n" "$RESPONSE" | jq -r ".config.interface.addresses.v6 // empty")

if [ -z "$CF_CLIENT_ID" ] || [ -z "$CF_PUBLIC_KEY" ] || [ -z "$CF_ADDR_V4" ]; then
  echo "WARP_REGISTRATION_FAILED"
  exit 1
fi

reserved=$(printf "%s" "$CF_CLIENT_ID" | base64 -d | od -An -v -t u1 | jq -s 'if length == 3 then . else error("bad reserved length") end')

jq --arg pk "$CF_PRIVATE_KEY" \
   --arg pub "$CF_PUBLIC_KEY" \
   --arg v4 "$CF_ADDR_V4/32" \
   --arg v6 "$CF_ADDR_V6/128" \
   --argjson res "$reserved" \
   ' .endpoints[0].private_key = $pk
   | .endpoints[0].address = [$v4, $v6]
   | .endpoints[0].peers[0].public_key = $pub
   | .endpoints[0].peers[0].reserved = $res ' \
   "$CONFIG" > "$TMP"

# Never replace a working config with invalid JSON.
jq empty "$TMP"

cp "$CONFIG" "$BAK" 2>/dev/null || true
mv "$TMP" "$CONFIG"

if ! kill -HUP 1 2>/dev/null; then
  # Roll back if the daemon did not accept the reload.
  cp "$BAK" "$CONFIG" 2>/dev/null || true
  kill -HUP 1 2>/dev/null || true
  echo "WARP_HOT_RELOAD_FAILED"
  exit 1
fi

echo "HOT_RELOAD_SUCCESS"
