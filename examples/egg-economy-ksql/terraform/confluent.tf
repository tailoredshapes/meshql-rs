# ===== Confluent Cloud Environment =====

resource "confluent_environment" "egg_economy" {
  display_name = var.environment_name
}

# ===== Kafka Cluster (Standard tier — RBAC required for ksqlDB) =====

resource "confluent_kafka_cluster" "main" {
  display_name = "egg-economy"
  availability = "SINGLE_ZONE"
  cloud        = var.confluent_cloud_provider
  region       = var.confluent_cloud_region

  standard {}

  environment {
    id = confluent_environment.egg_economy.id
  }
}

# ===== Admin Service Account (for Terraform-managed resources) =====

resource "confluent_service_account" "admin" {
  display_name = "egg-economy-admin"
  description  = "Admin service account for Terraform topic/ACL management"
}

resource "confluent_role_binding" "admin_cluster" {
  principal   = "User:${confluent_service_account.admin.id}"
  role_name   = "CloudClusterAdmin"
  crn_pattern = confluent_kafka_cluster.main.rbac_crn
}

resource "confluent_api_key" "admin_kafka" {
  display_name = "egg-economy-admin-kafka"
  description  = "Admin Kafka API key for Terraform"

  owner {
    id          = confluent_service_account.admin.id
    api_version = confluent_service_account.admin.api_version
    kind        = confluent_service_account.admin.kind
  }

  managed_resource {
    id          = confluent_kafka_cluster.main.id
    api_version = confluent_kafka_cluster.main.api_version
    kind        = confluent_kafka_cluster.main.kind

    environment {
      id = confluent_environment.egg_economy.id
    }
  }

  depends_on = [confluent_role_binding.admin_cluster]
}

# ===== Lambda Service Account =====

resource "confluent_service_account" "lambda" {
  display_name = "egg-economy-lambda"
  description  = "Service account for egg-economy-ksql Lambda function"
}

# ===== Lambda Kafka API Key =====

resource "confluent_api_key" "kafka" {
  display_name = "egg-economy-lambda-kafka"
  description  = "Kafka API key for Lambda"

  owner {
    id          = confluent_service_account.lambda.id
    api_version = confluent_service_account.lambda.api_version
    kind        = confluent_service_account.lambda.kind
  }

  managed_resource {
    id          = confluent_kafka_cluster.main.id
    api_version = confluent_kafka_cluster.main.api_version
    kind        = confluent_kafka_cluster.main.kind

    environment {
      id = confluent_environment.egg_economy.id
    }
  }
}

# ===== Kafka ACLs — allow Lambda service account full access =====
# (managed using admin API key)

resource "confluent_kafka_acl" "lambda_topic_write" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "WRITE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_read" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "READ"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_create" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "CREATE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_describe" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_describe_configs" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE_CONFIGS"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_alter" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "ALTER"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_alter_configs" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "ALTER_CONFIGS"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_topic_delete" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TOPIC"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DELETE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

# ===== Consumer group ACLs (wildcard for ksqlDB) =====

resource "confluent_kafka_acl" "lambda_group_all_read" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "GROUP"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "READ"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_group_all_describe" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "GROUP"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

# ===== ksqlDB internal consumer group ACLs =====

resource "confluent_kafka_acl" "lambda_group_read" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "GROUP"
  resource_name = "_confluent-ksql-"
  pattern_type  = "PREFIXED"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "READ"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_group_describe" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "GROUP"
  resource_name = "_confluent-ksql-"
  pattern_type  = "PREFIXED"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

# ===== ksqlDB transactional ID ACLs =====

resource "confluent_kafka_acl" "lambda_txn_write" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TRANSACTIONAL_ID"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "WRITE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_txn_describe" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "TRANSACTIONAL_ID"
  resource_name = "*"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

# ===== Cluster-level ACLs for ksqlDB =====

resource "confluent_kafka_acl" "lambda_cluster_describe_configs" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "CLUSTER"
  resource_name = "kafka-cluster"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE_CONFIGS"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_cluster_describe" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "CLUSTER"
  resource_name = "kafka-cluster"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "DESCRIBE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

resource "confluent_kafka_acl" "lambda_cluster_create" {
  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  resource_type = "CLUSTER"
  resource_name = "kafka-cluster"
  pattern_type  = "LITERAL"
  principal     = "User:${confluent_service_account.lambda.id}"
  host          = "*"
  operation     = "CREATE"
  permission    = "ALLOW"

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

# ===== Kafka Topics (13 entities) =====

locals {
  entities = toset([
    "farm", "coop", "hen", "container", "consumer",
    "lay_report", "storage_deposit", "storage_withdrawal",
    "container_transfer", "consumption_report",
    "container_inventory", "hen_productivity", "farm_output",
  ])
}

resource "confluent_kafka_topic" "entities" {
  for_each = local.entities

  topic_name       = each.key
  partitions_count = var.topic_partitions

  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  rest_endpoint = confluent_kafka_cluster.main.rest_endpoint

  credentials {
    key    = confluent_api_key.admin_kafka.id
    secret = confluent_api_key.admin_kafka.secret
  }
}

# ===== ksqlDB Cluster =====

resource "confluent_ksql_cluster" "main" {
  display_name = "egg-economy-ksql"
  csu          = 4

  kafka_cluster {
    id = confluent_kafka_cluster.main.id
  }

  credential_identity {
    id = confluent_service_account.lambda.id
  }

  environment {
    id = confluent_environment.egg_economy.id
  }

  depends_on = [
    confluent_kafka_acl.lambda_topic_write,
    confluent_kafka_acl.lambda_topic_read,
    confluent_kafka_acl.lambda_topic_create,
    confluent_kafka_acl.lambda_topic_describe,
    confluent_kafka_acl.lambda_topic_describe_configs,
    confluent_kafka_acl.lambda_group_read,
    confluent_kafka_acl.lambda_group_describe,
    confluent_kafka_acl.lambda_txn_write,
    confluent_kafka_acl.lambda_txn_describe,
    confluent_kafka_acl.lambda_cluster_describe_configs,
    confluent_kafka_acl.lambda_cluster_describe,
    confluent_kafka_acl.lambda_cluster_create,
  ]
}

# ===== Lambda ksqlDB Role Binding (requires Standard+ Kafka cluster) =====

resource "confluent_role_binding" "lambda_ksqldb" {
  principal   = "User:${confluent_service_account.lambda.id}"
  role_name   = "KsqlAdmin"
  crn_pattern = confluent_ksql_cluster.main.resource_name
}

# ===== ksqlDB API Key =====

resource "confluent_api_key" "ksqldb" {
  display_name = "egg-economy-lambda-ksqldb"
  description  = "ksqlDB API key for Lambda"

  owner {
    id          = confluent_service_account.lambda.id
    api_version = confluent_service_account.lambda.api_version
    kind        = confluent_service_account.lambda.kind
  }

  managed_resource {
    id          = confluent_ksql_cluster.main.id
    api_version = confluent_ksql_cluster.main.api_version
    kind        = confluent_ksql_cluster.main.kind

    environment {
      id = confluent_environment.egg_economy.id
    }
  }
}
