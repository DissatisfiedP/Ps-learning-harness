# Starts busybox httpd (it daemonizes) on LAB_PORT (default 8080) and waits
# until it listens. LAB_ROOT prefixes the web root (tests run this on the host).
set -e
R="${LAB_ROOT:-}"
PORT="${LAB_PORT:-8080}"
mkdir -p "$R/srv/www"
echo 'lab service' > "$R/srv/www/index.html"
busybox httpd -p "$PORT" -h "$R/srv/www" >/dev/null 2>&1 </dev/null
hex=$(printf '%04X' "$PORT")
i=0
until cat /proc/net/tcp /proc/net/tcp6 2>/dev/null | awk -v p="$hex" '$4 == "0A" { split($2, a, ":"); if (a[2] == p) f = 1 } END { exit !f }'; do
  i=$((i + 1))
  if [ "$i" -gt 50 ]; then
    echo "httpd did not start listening on port $PORT" >&2
    exit 1
  fi
  sleep 0.1 2>/dev/null || sleep 1
done
