# ===== IAM Role for Lambda =====

data "aws_iam_policy_document" "lambda_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "lambda" {
  name               = "egg-economy-ksql-lambda"
  assume_role_policy = data.aws_iam_policy_document.lambda_assume.json
}

resource "aws_iam_role_policy_attachment" "lambda_basic" {
  role       = aws_iam_role.lambda.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole"
}

# ===== Lambda Function =====

resource "aws_lambda_function" "egg_economy" {
  function_name = "egg-economy-ksql"
  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"
  memory_size   = var.lambda_memory_size
  timeout       = var.lambda_timeout
  role          = aws_iam_role.lambda.arn

  filename         = "${path.module}/../../../target/lambda/egg-economy-ksql/bootstrap.zip"
  source_code_hash = filebase64sha256("${path.module}/../../../target/lambda/egg-economy-ksql/bootstrap.zip")

  environment {
    variables = {
      CONFLUENT_KAFKA_REST_URL    = confluent_kafka_cluster.main.rest_endpoint
      CONFLUENT_KAFKA_CLUSTER_ID  = confluent_kafka_cluster.main.id
      CONFLUENT_KAFKA_API_KEY     = confluent_api_key.kafka.id
      CONFLUENT_KAFKA_API_SECRET  = confluent_api_key.kafka.secret
      CONFLUENT_KSQLDB_URL        = confluent_ksql_cluster.main.rest_endpoint
      CONFLUENT_KSQLDB_API_KEY    = confluent_api_key.ksqldb.id
      CONFLUENT_KSQLDB_API_SECRET = confluent_api_key.ksqldb.secret
    }
  }
}

# ===== API Gateway HTTP API =====

resource "aws_apigatewayv2_api" "api" {
  name          = "egg-economy-ksql"
  protocol_type = "HTTP"
}

resource "aws_apigatewayv2_integration" "lambda" {
  api_id                 = aws_apigatewayv2_api.api.id
  integration_type       = "AWS_PROXY"
  integration_uri        = aws_lambda_function.egg_economy.invoke_arn
  payload_format_version = "2.0"
}

resource "aws_apigatewayv2_route" "proxy" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "ANY /{proxy+}"
  target    = "integrations/${aws_apigatewayv2_integration.lambda.id}"
}

resource "aws_apigatewayv2_stage" "default" {
  api_id      = aws_apigatewayv2_api.api.id
  name        = "$default"
  auto_deploy = true
}

resource "aws_lambda_permission" "apigw" {
  statement_id  = "AllowAPIGatewayInvoke"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.egg_economy.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/*/*"
}
