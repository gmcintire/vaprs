#!/bin/bash
# Set up APT repository on VPS for vaprs
# Run this as graham on your VPS (vaprs.w5isp.com)
#
# Prerequisites:
#   - Caddy already running and configured to serve vaprs.w5isp.com
#     from /home/graham/apps/vaprs
#   - DNS pointing vaprs.w5isp.com to VPS
#
# Usage: bash apt-repo-setup.sh

set -euo pipefail

REPO_DIR="$HOME/apps/vaprs"
INCOMING_DIR="$HOME/apps/vaprs-incoming"
GPG_NAME="vaprs APT repo <graham@w5isp.com>"

echo "=== Installing reprepro ==="
sudo apt-get update
sudo apt-get install -y reprepro gpg

echo "=== Generating GPG signing key ==="
if ! gpg --list-keys "$GPG_NAME" &>/dev/null; then
    gpg --batch --gen-key <<GPGEOF
%no-protection
Key-Type: RSA
Key-Length: 4096
Name-Real: vaprs APT repo
Name-Email: graham@w5isp.com
Expire-Date: 0
%commit
GPGEOF
    echo "GPG key generated."
else
    echo "GPG key already exists."
fi

echo "=== Setting up reprepro ==="
mkdir -p "$REPO_DIR"/{conf,dists,pool,db}
mkdir -p "$INCOMING_DIR"

cat > "$REPO_DIR/conf/distributions" <<EOF
Origin: vaprs
Label: vaprs
Codename: stable
Architectures: amd64 arm64 armhf source
Components: main
Description: vaprs APRS iGate and digipeater
SignWith: yes
EOF

cat > "$REPO_DIR/conf/options" <<EOF
verbose
basedir $REPO_DIR
EOF

# Export public key for users
gpg --armor --export "$GPG_NAME" > "$REPO_DIR/gpg.key"

echo "=== Creating import script ==="
cat > "$HOME/import-debs.sh" <<SCRIPT
#!/bin/bash
# Import all .deb files from incoming/ into the repo
set -euo pipefail
INCOMING="$INCOMING_DIR"
REPO="$REPO_DIR"

for deb in "\$INCOMING"/*.deb; do
    [ -f "\$deb" ] || continue
    echo "Importing: \$deb"
    reprepro -b "\$REPO" includedeb stable "\$deb"
    rm "\$deb"
done
echo "Done."
SCRIPT
chmod +x "$HOME/import-debs.sh"

echo ""
echo "=== Setup complete ==="
echo ""
echo "Caddy config needed (add to Caddyfile):"
echo ""
echo "  vaprs.w5isp.com {"
echo "    root * $REPO_DIR"
echo "    file_server browse"
echo "  }"
echo ""
echo "Then: sudo systemctl reload caddy"
echo ""
echo "GitHub secrets needed:"
echo "  - DEPLOY_SSH_KEY: private SSH key for graham@your-vps"
echo "  - DEPLOY_HOST: vaprs.w5isp.com"
echo ""
echo "User install command:"
echo '  curl -fsSL https://vaprs.w5isp.com/gpg.key | sudo gpg --dearmor -o /usr/share/keyrings/vaprs.gpg'
echo '  echo "deb [signed-by=/usr/share/keyrings/vaprs.gpg] https://vaprs.w5isp.com stable main" | sudo tee /etc/apt/sources.list.d/vaprs.list'
echo '  sudo apt update && sudo apt install vaprs'
