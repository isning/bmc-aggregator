#!/usr/bin/env bash
set -euo pipefail

# Build One-KVM frontend with runtime configurable API_BASE and router base
# Prerequisites: node, npm, git

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
FRONTEND_DIR="$PROJECT_DIR/frontend"
TMP_DIR="/tmp/bmc-aggregator-build"
REPO_URL="https://github.com/mofeng-git/One-KVM.git"

echo "==> Cloning One-KVM frontend..."
rm -rf "$TMP_DIR"
git clone --depth 1 --filter=blob:none "$REPO_URL" "$TMP_DIR"

echo "==> Applying frontend patches..."

# 1. index.html: add config.js script tag before </head>
sed -i 's|</head>|<script src="config.js"></script>\n</head>|' "$TMP_DIR/web/index.html"

# 2. api/index.ts: use __KVM_API_BASE global
sed -i "s|^const API_BASE = '/api'$|const API_BASE = (window as any).__KVM_API_BASE ?? '/api'|" "$TMP_DIR/web/src/api/index.ts"

# 3. api/request.ts: use __KVM_API_BASE global
sed -i "s|^const API_BASE = '/api'$|const API_BASE = (window as any).__KVM_API_BASE ?? '/api'|" "$TMP_DIR/web/src/api/request.ts"

# 4. router/index.ts: add __KVM_ROUTER_BASE support
sed -i "s|^const router = createRouter({\$|const routerBase = (window as any).__KVM_ROUTER_BASE ?? '/'\nconst router = createRouter({|" "$TMP_DIR/web/src/router/index.ts"
sed -i "s|^  history: createWebHistory(),|  history: createWebHistory(routerBase),|" "$TMP_DIR/web/src/router/index.ts"

# 5. types/websocket.ts: use __KVM_API_BASE for WebSocket path mapping
sed -i 's|return `${protocol}//${window.location.host}${path}`|const apiBase = (window as any).__KVM_API_BASE ?? '"'"'/api'"'"'\n  const mappedPath = path.startsWith('"'"'/api'"'"') ? apiBase + path.slice(4) : path\n  return `${protocol}//${window.location.host}${mappedPath}`|' "$TMP_DIR/web/src/types/websocket.ts"

# 6. auth store: always consider user authenticated (no backend login needed)
sed -i 's|const isAuthenticated = ref(false)|const isAuthenticated = ref(true)|' "$TMP_DIR/web/src/stores/auth.ts"
sed -i 's|const initialized = ref(false)|const initialized = ref(true)|' "$TMP_DIR/web/src/stores/auth.ts"
sed -i 's|const needsSetup = ref(false)|const needsSetup = ref(false)|' "$TMP_DIR/web/src/stores/auth.ts"
# Make checkAuth always succeed (bypass call to backend)
sed -i '/async function checkAuth() {/,/^  }/c\
  async function checkAuth() {\
    isAuthenticated.value = true\
    user.value = "bmc"\
    return { authenticated: true, user: "bmc" as string | undefined }\
  }' "$TMP_DIR/web/src/stores/auth.ts"

echo "==> Installing dependencies..."
cd "$TMP_DIR/web"
npm install

echo "==> Building frontend..."
npm run build -- --base=./ 2>&1

echo "==> Copying build output..."
rm -rf "$FRONTEND_DIR"
cp -r "$TMP_DIR/web/dist" "$FRONTEND_DIR"
echo "Frontend built at $FRONTEND_DIR"

echo "==> Done!"
