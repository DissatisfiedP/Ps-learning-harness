# Lab setup. LAB_ROOT (default empty) prefixes every path and LAB_OWNER
# (default root) is the owner the file must end up with, so tests can run
# this on the host; in the container neither is set.
set -e
R="${LAB_ROOT:-}"
OWNER="${LAB_OWNER:-root}"
mkdir -p "$R/srv/app"
printf 'db_user=app\ndb_password=correct-horse-battery\napi_token=3f9c1d7a\n' > "$R/srv/app/secret.conf"
chown "$OWNER" "$R/srv/app/secret.conf"
chmod 666 "$R/srv/app/secret.conf"
