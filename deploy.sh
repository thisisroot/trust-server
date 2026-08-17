#!/usr/bin/env bash
#
# Trust backend deploy / redeploy for a single Ubuntu VPS behind nginx.
#
# Idempotent: the FIRST run provisions everything (swap, Rust, build tools, the
# systemd service, the nginx reverse-proxy site, TLS). EVERY run re-fetches the
# latest source, rebuilds, swaps the binary, and restarts — so the same command
# is both "deploy" and "redeploy".
#
# Run ON THE VPS as root:
#   ./deploy.sh
#   DOMAIN=trust.zeddm.ir REPO_OWNER=thisisroot CERTBOT_EMAIL=you@x.com ./deploy.sh
#
# It fetches source via GitHub's codeload tarball (plain HTTPS) because git
# smart-HTTP hangs on this host. The repo must be public, or set REPO_TARBALL_URL
# to a private/authenticated tarball URL.
set -euo pipefail

# ── config (override via env) ────────────────────────────────────────────────
DOMAIN="${DOMAIN:-trust.zeddm.ir}"
REPO_OWNER="${REPO_OWNER:-thisisroot}"
REPO_NAME="${REPO_NAME:-trust-server}"
BRANCH="${BRANCH:-main}"
CERTBOT_EMAIL="${CERTBOT_EMAIL:-}"
REPO_TARBALL_URL="${REPO_TARBALL_URL:-https://codeload.github.com/${REPO_OWNER}/${REPO_NAME}/tar.gz/refs/heads/${BRANCH}}"

SRC_DIR="/root/trust-server-src"
APP_DIR="/opt/trust-server"
DATABASE_URL="${DATABASE_URL:-postgres://trust:trust@localhost:5432/trust}"
DB_NAME="${DB_NAME:-trust}"
DB_USER="${DB_USER:-trust}"
DB_PASS="${DB_PASS:-trust}"
SERVICE="/etc/systemd/system/trust-server.service"
NGINX_SITE="/etc/nginx/sites-available/trust.conf"
NGINX_LINK="/etc/nginx/sites-enabled/trust.conf"
WS_MAP="/etc/nginx/conf.d/ws_upgrade.conf"

log() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }

[ "$(id -u)" = 0 ] || { echo "Run as root."; exit 1; }

ensure_swap() {
  if ! swapon --show 2>/dev/null | grep -q .; then
    local mb; mb=$(free -m | awk '/Mem/{print $2}')
    if [ "${mb:-9999}" -lt 1500 ]; then
      log "Low RAM (${mb}MB) — adding 2G swap for the build"
      fallocate -l 2G /swapfile 2>/dev/null || dd if=/dev/zero of=/swapfile bs=1M count=2048
      chmod 600 /swapfile; mkswap /swapfile; swapon /swapfile
      grep -q '/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
    fi
  fi
}

ensure_packages() {
  log "Ensuring build + serving prerequisites"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq build-essential pkg-config curl ca-certificates \
    nginx certbot python3-certbot-nginx postgresql
}

ensure_postgres() {
  log "Ensuring Postgres database + role"
  systemctl enable --now postgresql
  # Idempotent role + database creation (localhost-only; not exposed publicly).
  sudo -u postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname='${DB_USER}'" | grep -q 1 \
    || sudo -u postgres psql -c "CREATE ROLE ${DB_USER} LOGIN PASSWORD '${DB_PASS}'"
  sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname='${DB_NAME}'" | grep -q 1 \
    || sudo -u postgres createdb -O "${DB_USER}" "${DB_NAME}"
}

ensure_rust() {
  if ! command -v cargo >/dev/null 2>&1 && [ ! -f "$HOME/.cargo/env" ]; then
    log "Installing Rust"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  fi
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
}

fetch_source() {
  log "Fetching source: $REPO_TARBALL_URL"
  rm -rf "$SRC_DIR" /tmp/trust-src.tgz
  curl -fsSL -o /tmp/trust-src.tgz "$REPO_TARBALL_URL"
  mkdir -p "$SRC_DIR"
  tar xzf /tmp/trust-src.tgz -C "$SRC_DIR" --strip-components=1
}

build() {
  log "Building release (-j1 to stay within RAM; slow on small boxes)"
  ( cd "$SRC_DIR" && cargo build --release -j 1 )
}

install_service() {
  log "Installing binary + systemd service"
  id trust >/dev/null 2>&1 || useradd --system --home "$APP_DIR" --shell /usr/sbin/nologin trust
  mkdir -p "$APP_DIR"
  systemctl stop trust-server 2>/dev/null || true   # release the (locked) binary
  install -m 0755 "$SRC_DIR/target/release/trust-server" "$APP_DIR/trust-server"
  chown -R trust:trust "$APP_DIR"
  # Always (re)write the unit so config changes (e.g. DATABASE_URL) take effect on redeploy.
  cat > "$SERVICE" <<EOF
[Unit]
Description=Trust server (axum HTTP + WebSocket)
After=network.target postgresql.service
Wants=postgresql.service

[Service]
User=trust
Group=trust
WorkingDirectory=$APP_DIR
Environment=TRUST_BIND_ADDR=127.0.0.1:8080
Environment=TRUST_DATABASE_URL=${DATABASE_URL}
Environment=RUST_LOG=info
ExecStart=$APP_DIR/trust-server
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable trust-server
  systemctl restart trust-server
}

install_nginx() {
  log "Configuring nginx reverse proxy for $DOMAIN"
  [ -f "$WS_MAP" ] || cat > "$WS_MAP" <<'EOF'
map $http_upgrade $connection_upgrade { default upgrade; '' close; }
EOF
  if [ ! -f "$NGINX_SITE" ]; then
    cat > "$NGINX_SITE" <<EOF
server {
    listen 80;
    server_name $DOMAIN;
    client_max_body_size 25m;
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host              \$host;
        proxy_set_header X-Real-IP         \$remote_addr;
        proxy_set_header X-Forwarded-For   \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_set_header Upgrade    \$http_upgrade;
        proxy_set_header Connection \$connection_upgrade;
        proxy_read_timeout  3600s;
        proxy_send_timeout  3600s;
    }
}
EOF
    ln -sf "$NGINX_SITE" "$NGINX_LINK"
  fi
  nginx -t
  systemctl reload nginx
}

ensure_tls() {
  if [ ! -d "/etc/letsencrypt/live/$DOMAIN" ]; then
    log "Obtaining TLS certificate for $DOMAIN"
    if [ -n "$CERTBOT_EMAIL" ]; then
      certbot --nginx -d "$DOMAIN" --non-interactive --agree-tos -m "$CERTBOT_EMAIL" --redirect
    else
      certbot --nginx -d "$DOMAIN" --non-interactive --agree-tos \
        --register-unsafely-without-email --redirect
    fi
  else
    log "TLS certificate already present — skipping (certbot auto-renews)"
  fi
}

verify() {
  log "Verifying"
  sleep 1
  echo -n "local  : "; curl -s http://127.0.0.1:8080/health; echo
  echo -n "public : "; curl -s "https://$DOMAIN/health" || echo "(not reachable yet — check DNS/TLS)"; echo
}

ensure_swap
ensure_packages
ensure_postgres
ensure_rust
fetch_source
build
install_service
install_nginx
ensure_tls
verify
log "Done → https://$DOMAIN"
