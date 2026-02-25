#!/usr/bin/env bash
set -euo pipefail

# Build static Linux binary (x86_64 musl for Azure Functions)
echo "==> Building farm-azure..."
cargo build --release --target x86_64-unknown-linux-musl -p farm-azure

# Package for Azure Functions deployment
echo "==> Packaging for Azure Functions..."
STAGE_DIR=$(mktemp -d)
cp target/x86_64-unknown-linux-musl/release/farm-azure "$STAGE_DIR/handler"
cp azure-functions/host.json "$STAGE_DIR/"
cp -r azure-functions/api "$STAGE_DIR/"

cd "$STAGE_DIR"
zip -r /tmp/farm-azure-deploy.zip .
cd -
rm -rf "$STAGE_DIR"

# Deploy
FUNC_NAME=$(cd terraform && terraform output -raw function_app_name)
RG_NAME=$(cd terraform && terraform output -raw resource_group)

echo "==> Deploying to Azure Function: $FUNC_NAME..."
az functionapp deployment source config-zip \
  --name "$FUNC_NAME" \
  --resource-group "$RG_NAME" \
  --src /tmp/farm-azure-deploy.zip

echo "==> Done!"
FUNC_URL=$(cd terraform && terraform output -raw function_app_url)
echo "API available at: $FUNC_URL"
echo ""
echo "Test:"
echo "  curl -s $FUNC_URL/api/farm/api -X POST -H 'Content-Type: application/json' -d '{\"name\": \"Azure Farm\"}'"
echo "  curl -s $FUNC_URL/api/farm/graph -X POST -H 'Content-Type: application/json' -d '{\"query\": \"{ getFarms { id name } }\"}'"
