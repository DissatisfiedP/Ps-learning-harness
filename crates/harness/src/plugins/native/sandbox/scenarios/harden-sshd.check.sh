R="${LAB_ROOT:-}"
C="$R/etc/ssh/sshd_config"
if [ ! -f "$C" ]; then
  echo "fail: /etc/ssh/sshd_config is missing"
  exit 1
fi
# sshd uses the first active occurrence of a keyword.
value() {
  awk -v k="$1" 'BEGIN { k = tolower(k) } { sub(/#.*/, "") } NF >= 2 && tolower($1) == k { print tolower($2); exit }' "$C"
}
bad=0
for key in PermitRootLogin PasswordAuthentication; do
  v=$(value "$key")
  if [ "$v" = no ]; then
    echo "ok: $key is no"
  else
    echo "fail: $key is ${v:-not set}, not no"
    bad=1
  fi
done
if awk '{ sub(/#.*/, "") } NF >= 2 && tolower($1) == "port" && $2 == 22 { f = 1 } END { exit !f }' "$C"; then
  echo "ok: Port 22 is still set"
else
  echo "fail: the Port 22 line is gone or changed"
  bad=1
fi
left=$(awk '{ sub(/#.*/, "") } { for (i = 2; i <= NF; i++) if (tolower($i) == "yes") { print $1; break } }' "$C" | tr '\n' ' ')
if [ -z "$left" ]; then
  echo "ok: no setting is still yes"
else
  echo "fail: still yes: $left"
  bad=1
fi
exit $bad
