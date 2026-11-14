R="${LAB_ROOT:-}"
F="$R/tmp/flag.txt"
want="FLAG{$LAB_FLAG}"
if [ ! -f "$F" ]; then
  echo "fail: /tmp/flag.txt doesn't exist"
  exit 1
fi
got=$(cat "$F")
if [ "$got" = "$want" ]; then
  echo "ok: /tmp/flag.txt holds the flag"
  exit 0
fi
case "$got" in
  *"$want"*) echo "fail: /tmp/flag.txt holds the flag with other text around it; it must hold exactly the flag" ;;
  "") echo "fail: /tmp/flag.txt is empty" ;;
  *) echo "fail: /tmp/flag.txt doesn't hold the flag" ;;
esac
exit 1
