# Six logs under /var/log/app; one (picked from LAB_FLAG) holds a line with
# FLAG{<LAB_FLAG>}. LAB_ROOT prefixes the paths (tests run this on the host).
set -e
: "${LAB_FLAG:?LAB_FLAG is not set}"
R="${LAB_ROOT:-}"
D="$R/var/log/app"
mkdir -p "$D" "$R/tmp"
rm -f "$R/tmp/flag.txt"
n=$(printf '%s' "$LAB_FLAG" | cksum | cut -d ' ' -f 1)
pick=$((n % 6))
i=0
for name in access auth worker scheduler mailer audit; do
  awk -v name="$name" -v flag="$LAB_FLAG" -v hide="$([ "$i" -eq "$pick" ] && echo 1 || echo 0)" -v seed="$((n + i))" '
    BEGIN {
      srand(seed)
      split("started request handled|cache refreshed|connection reset by peer|retrying after backoff|queue drained|config reloaded|slow query (412 ms)|session opened|session closed|token refreshed|health probe ok|feature flag cache warmed", msg, "|")
      split("INFO|INFO|INFO|WARN|DEBUG|ERROR", lvl, "|")
      at = 20 + int(rand() * 40)
      for (k = 1; k <= 90; k++) {
        printf "2025-03-%02d %02d:%02d:%02d %-5s %s: %s\n", 1 + int(k / 12), int(rand() * 24), int(rand() * 60), int(rand() * 60), lvl[1 + int(rand() * 6)], name, msg[1 + int(rand() * 12)]
        if (hide == 1 && k == at) {
          printf "2025-03-%02d %02d:%02d:%02d INFO  %s: support bundle key FLAG{%s}\n", 1 + int(k / 12), int(rand() * 24), int(rand() * 60), int(rand() * 60), name, flag
        }
      }
    }' > "$D/$name.log"
  i=$((i + 1))
done
