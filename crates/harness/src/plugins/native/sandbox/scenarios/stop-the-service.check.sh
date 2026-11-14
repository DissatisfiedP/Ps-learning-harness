PORT="${LAB_PORT:-8080}"
hex=$(printf '%04X' "$PORT")
listening() {
  cat /proc/net/tcp /proc/net/tcp6 2>/dev/null | awk -v p="$hex" '$4 == "0A" { split($2, a, ":"); if (a[2] == p) f = 1 } END { exit !f }'
}
# A service that was just signalled needs a moment to close its socket.
i=0
while listening && [ "$i" -lt 10 ]; do
  i=$((i + 1))
  sleep 0.1 2>/dev/null || sleep 1
done
if listening; then
  echo "fail: something still listens on port $PORT"
  exit 1
fi
echo "ok: nothing listens on port $PORT"
