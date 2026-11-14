R="${LAB_ROOT:-}"
OWNER="${LAB_OWNER:-root}"
F="$R/srv/app/secret.conf"
if [ ! -f "$F" ]; then
  echo "fail: /srv/app/secret.conf is missing; it has to stay where it is"
  exit 1
fi
set -- $(stat -c '%a %U' "$F")
mode=$1
owner=$2
bad=0
if [ "$mode" = 600 ]; then
  echo "ok: the mode is 600"
else
  echo "fail: the mode is $mode, not 600"
  bad=1
fi
if [ "$owner" = "$OWNER" ]; then
  echo "ok: the owner is $OWNER"
else
  echo "fail: the owner is $owner, not $OWNER"
  bad=1
fi
if grep -q '^db_password=correct-horse-battery$' "$F"; then
  echo "ok: the contents are unchanged"
else
  echo "fail: the file's contents were changed"
  bad=1
fi
exit $bad
