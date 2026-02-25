terraform {
  required_providers {
    azurerm = {
      source  = "hashicorp/azurerm"
      version = "~> 4.0"
    }
  }
}

provider "azurerm" {
  features {}
  subscription_id                = var.subscription_id
  resource_provider_registrations = "none"
}

locals {
  prefix = "${var.project}-${var.env}-${var.region}"
  # Storage account names: alphanumeric only, max 24 chars
  st_prefix = "${var.project}${var.env}${var.region}"
}

# ===== RESOURCE GROUP (referenced, not created — allows org naming policies) =====

data "azurerm_resource_group" "rg" {
  name = var.resource_group_name
}

# ===== VNET (required for VNet integration) =====

resource "azurerm_virtual_network" "vnet" {
  name                = "${local.prefix}-farmaz-vnet"
  location            = data.azurerm_resource_group.rg.location
  resource_group_name = data.azurerm_resource_group.rg.name
  address_space       = ["10.0.0.0/16"]
}

resource "azurerm_subnet" "functions" {
  name                 = "${local.prefix}-farmaz-snet"
  resource_group_name  = data.azurerm_resource_group.rg.name
  virtual_network_name = azurerm_virtual_network.vnet.name
  address_prefixes     = ["10.0.1.0/24"]

  delegation {
    name = "functions-delegation"
    service_delegation {
      name = "Microsoft.Web/serverFarms"
      actions = [
        "Microsoft.Network/virtualNetworks/subnets/action",
      ]
    }
  }
}

# ===== AZURE FILES (SMB — merkql storage, mountable by Functions) =====

resource "azurerm_storage_account" "merkql" {
  name                     = "${local.st_prefix}merkqlstf1"
  location                 = data.azurerm_resource_group.rg.location
  resource_group_name      = data.azurerm_resource_group.rg.name
  account_tier             = "Standard"
  account_replication_type = "LRS"
}

resource "azurerm_storage_share" "merkql" {
  name               = "merkql"
  storage_account_id = azurerm_storage_account.merkql.id
  quota              = 5 # GB
}

# ===== FUNCTION APP STORAGE (runtime state) =====

resource "azurerm_storage_account" "functions" {
  name                     = "${local.st_prefix}farmfnst1"
  location                 = data.azurerm_resource_group.rg.location
  resource_group_name      = data.azurerm_resource_group.rg.name
  account_tier             = "Standard"
  account_replication_type = "LRS"
}

# ===== APP SERVICE PLAN (Elastic Premium — required for VNet + file mount) =====

resource "azurerm_service_plan" "plan" {
  name                = "${local.prefix}-farmaz-func"
  location            = data.azurerm_resource_group.rg.location
  resource_group_name = data.azurerm_resource_group.rg.name
  os_type             = "Linux"
  sku_name            = "EP1"
}

# ===== FUNCTION APP =====

resource "azurerm_linux_function_app" "app" {
  name                       = "${local.prefix}-farmaz-func"
  location                   = data.azurerm_resource_group.rg.location
  resource_group_name        = data.azurerm_resource_group.rg.name
  service_plan_id            = azurerm_service_plan.plan.id
  storage_account_name       = azurerm_storage_account.functions.name
  storage_account_access_key = azurerm_storage_account.functions.primary_access_key

  virtual_network_subnet_id = azurerm_subnet.functions.id

  site_config {
    application_stack {
      use_custom_runtime = true
    }
  }

  app_settings = {
    "MERKQL_DATA_PATH"         = "/mnt/merkql"
    "FUNCTIONS_WORKER_RUNTIME" = "custom"
    "WEBSITE_RUN_FROM_PACKAGE" = "1"
    "WEBSITE_MOUNT_ENABLED"    = "1"
  }

  storage_account {
    access_key   = azurerm_storage_account.merkql.primary_access_key
    account_name = azurerm_storage_account.merkql.name
    name         = "merkqlmount"
    share_name   = azurerm_storage_share.merkql.name
    type         = "AzureFiles"
    mount_path   = "/mnt/merkql"
  }
}

# ===== OUTPUTS =====

output "function_app_url" {
  value = "https://${azurerm_linux_function_app.app.default_hostname}"
}

output "function_app_name" {
  value = azurerm_linux_function_app.app.name
}

output "resource_group" {
  value = data.azurerm_resource_group.rg.name
}
