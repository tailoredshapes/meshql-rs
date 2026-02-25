# farm-azure

meshql-rs on Azure Functions with merkql (Azure Files SMB).

Same 4-entity farm model (Farm > Coop > Hen > LayReport) running as an Azure Functions custom handler backed by merkql on Azure Files SMB.

## Prerequisites

- Rust with musl target: `rustup target add x86_64-unknown-linux-musl`
- Azure CLI: `az login`
- Terraform

## Deploy

```bash
# 1. Create a resource group (name must comply with your org's naming policy)
az group create --name <your-rg-name> --location eastus2

# 2. Provision Azure infrastructure
cd terraform
terraform init
terraform apply \
  -var 'subscription_id=<your-sub-id>' \
  -var 'resource_group_name=<your-rg-name>' \
  -var 'project=farm' \
  -var 'env=dev' \
  -var 'region=eus2'

# 3. Build and deploy
cd ..
./deploy.sh
```

## Local Development

```bash
# Run locally (uses ./data/ for merkql storage)
MERKQL_DATA_PATH=./data cargo run -p farm-azure

# Test
curl -s localhost:3000/farm/api -X POST -H 'Content-Type: application/json' \
  -d '{"name": "Test Farm", "address": "123 Main St"}'

curl -s localhost:3000/farm/graph -X POST -H 'Content-Type: application/json' \
  -d '{"query": "{ getFarms { id name coops { id name hens { id name } } } }"}'
```

## Architecture

```
Azure Functions (EP1 Premium Plan)
  └── Custom Handler (Rust binary)
        └── meshql-server (axum on FUNCTIONS_CUSTOMHANDLER_PORT)
              ├── /farm/api     POST/PUT  (REST)
              ├── /farm/graph   POST      (GraphQL)
              ├── /coop/api     ...
              ├── /coop/graph   ...
              ├── /hen/api      ...
              ├── /hen/graph    ...
              ├── /lay_report/api   ...
              └── /lay_report/graph ...

Azure Files (SMB)
  └── mounted at /mnt/merkql
        └── merkql append-only event log
```

The catch-all HTTP trigger (`api/function.json`) forwards all paths to the custom handler. Azure Functions manages scaling, and the SMB mount gives all instances shared access to the merkql data directory.

## Naming Convention

Resources follow a `{project}-{env}-{region}-{purpose}` naming pattern. Storage accounts use the same segments without hyphens (Azure requires alphanumeric only). Set `project`, `env`, and `region` terraform variables to match your org's conventions.

## API

### REST

| Route | Method | Purpose |
|-------|--------|---------|
| `/farm/api` | POST | Create farm |
| `/farm/api/{id}` | PUT | Update farm |
| `/coop/api` | POST | Create coop |
| `/hen/api` | POST | Create hen |
| `/lay_report/api` | POST | Create lay report |

### GraphQL

| Endpoint | Queries |
|----------|---------|
| `/farm/graph` | `getFarm(id)`, `getFarms` |
| `/coop/graph` | `getCoop(id)`, `getCoops`, `getCoopsByFarm(id)` |
| `/hen/graph` | `getHen(id)`, `getHens`, `getHensByCoop(id)` |
| `/lay_report/graph` | `getLayReport(id)`, `getLayReports`, `getLayReportsByHen(id)` |

## Cost

- **EP1 plan**: ~$0.169/hour (~$123/month) — required for VNet + file mount
- **Azure Files Standard 5GB SMB**: ~$0.30/month
- **VNet**: Free

For a cheaper option, use `meshql-ksql` with Confluent Cloud (consumption plan, no VNet needed).
