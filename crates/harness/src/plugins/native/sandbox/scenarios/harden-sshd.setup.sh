# LAB_ROOT prefixes the path (tests run this on the host).
set -e
R="${LAB_ROOT:-}"
mkdir -p "$R/etc/ssh"
cat > "$R/etc/ssh/sshd_config" <<'CONF'
# OpenSSH server configuration (lab copy)
Port 22
#AddressFamily any
ListenAddress 0.0.0.0

# Authentication
LoginGraceTime 2m
PermitRootLogin yes
#PermitRootLogin prohibit-password
MaxAuthTries 6
PasswordAuthentication yes
PermitEmptyPasswords yes

# Forwarding
X11Forwarding yes
AllowTcpForwarding no

Subsystem sftp /usr/lib/ssh/sftp-server
CONF
