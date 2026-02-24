#!/bin/bash
# Set up APT repository on VPS for vaprs
# Run this once on your VPS (vaprs.w5isp.com)
#
# Prerequisites: root access, DNS pointing vaprs.w5isp.com to VPS
#
# Usage: sudo bash apt-repo-setup.sh

set -euo pipefail

REPO_DIR="/var/www/vaprs"
GPG_NAME="vaprs APT repo <graham@w5isp.com>"

echo "=== Installing dependencies ==="
apt-get update
apt-get install -y reprepro nginx gpg

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

cat > "$REPO_DIR/conf/distributions" <<'EOF'
Origin: vaprs
Label: vaprs
Codename: stable
Architectures: amd64 arm64 armhf source
Components: main
Description: vaprs APRS iGate and digipeater
SignWith: yes
EOF

cat > "$REPO_DIR/conf/options" <<'EOF'
verbose
basedir /var/www/vaprs
EOF

# Export public key for users
gpg --armor --export "$GPG_NAME" > "$REPO_DIR/gpg.key"

echo "=== Setting up nginx ==="
cat > /etc/nginx/sites-available/vaprs <<'NGINX'
server {
    listen 80;
    server_name vaprs.w5isp.com;

    root /var/www/vaprs;
    autoindex on;

    location / {
        try_files $uri $uri/ =404;
    }
}
NGINX

ln -sf /etc/nginx/sites-available/vaprs /etc/nginx/sites-enabled/vaprs
nginx -t && systemctl reload nginx

echo "=== Setting up deploy user ==="
if ! id vaprs-deploy &>/dev/null; then
    useradd -r -m -s /bin/bash vaprs-deploy
fi
mkdir -p /home/vaprs-deploy/.ssh /home/vaprs-deploy/incoming
chown -R vaprs-deploy:vaprs-deploy /home/vaprs-deploy
chown -R vaprs-deploy:vaprs-deploy "$REPO_DIR"

# Import GPG key for deploy user
sudo -u vaprs-deploy gpg --import <(gpg --armor --export-secret-keys "$GPG_NAME")

echo "=== Creating import script ==="
cat > /home/vaprs-deploy/import-debs.sh <<'SCRIPT'
#!/bin/bash
# Import all .deb files from incoming/ into the repo
set -euo pipefail
INCOMING="/home/vaprs-deploy/incoming"
REPO="/var/www/vaprs"

for deb in "$INCOMING"/*.deb; do
    [ -f "$deb" ] || continue
    echo "Importing: $deb"
    reprepro -b "$REPO" includedeb stable "$deb"
    rm "$deb"
done
echo "Done."
SCRIPT
chmod +x /home/vaprs-deploy/import-debs.sh
chown vaprs-deploy:vaprs-deploy /home/vaprs-deploy/import-debs.sh

echo ""
echo "=== Setup complete ==="
echo ""
echo "Next steps:"
echo "  1. Add SSH public key to /home/vaprs-deploy/.ssh/authorized_keys"
echo "  2. Set up TLS with: certbot --nginx -d vaprs.w5isp.com"
echo "  3. Add these GitHub secrets:"
echo "     - DEPLOY_SSH_KEY: private SSH key for vaprs-deploy user"
echo "     - DEPLOY_HOST: vaprs.w5isp.com"
echo ""
echo "User install command:"
echo '  curl -fsSL https://vaprs.w5isp.com/gpg.key | sudo gpg --dearmor -o /usr/share/keyrings/vaprs.gpg'
echo '  echo "deb [signed-by=/usr/share/keyrings/vaprs.gpg] https://vaprs.w5isp.com stable main" | sudo tee /etc/apt/sources.list.d/vaprs.list'
echo '  sudo apt update && sudo apt install vaprs'
