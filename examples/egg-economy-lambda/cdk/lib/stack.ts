import * as cdk from "aws-cdk-lib";
import * as ec2 from "aws-cdk-lib/aws-ec2";
import * as efs from "aws-cdk-lib/aws-efs";
import * as lambda from "aws-cdk-lib/aws-lambda";
import * as apigw from "aws-cdk-lib/aws-apigatewayv2";
import * as integrations from "aws-cdk-lib/aws-apigatewayv2-integrations";
import { Construct } from "constructs";

export class EggEconomyLambdaStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    // VPC with public + private subnets
    const vpc = new ec2.Vpc(this, "Vpc", {
      maxAzs: 2,
      natGateways: 1,
    });

    // EFS file system
    const fileSystem = new efs.FileSystem(this, "MerkqlFS", {
      vpc,
      encrypted: true,
      performanceMode: efs.PerformanceMode.GENERAL_PURPOSE,
      throughputMode: efs.ThroughputMode.ELASTIC,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });

    // EFS access point for Lambda
    const accessPoint = fileSystem.addAccessPoint("LambdaAP", {
      path: "/merkql",
      createAcl: {
        ownerGid: "1001",
        ownerUid: "1001",
        permissions: "755",
      },
      posixUser: {
        gid: "1001",
        uid: "1001",
      },
    });

    // Lambda function
    const fn_ = new lambda.Function(this, "EggEconomy", {
      runtime: lambda.Runtime.PROVIDED_AL2023,
      architecture: lambda.Architecture.ARM_64,
      handler: "bootstrap",
      code: lambda.Code.fromAsset("../target/lambda/egg-economy-lambda/"),
      memorySize: 512,
      timeout: cdk.Duration.seconds(30),
      filesystem: lambda.FileSystem.fromEfsAccessPoint(accessPoint, "/mnt/efs"),
      vpc,
      vpcSubnets: { subnetType: ec2.SubnetType.PRIVATE_WITH_EGRESS },
      environment: {
        EFS_MOUNT_PATH: "/mnt/efs",
      },
    });

    // API Gateway HTTP API
    const api = new apigw.HttpApi(this, "Api", {
      apiName: "EggEconomyApi",
    });

    const integration = new integrations.HttpLambdaIntegration(
      "LambdaIntegration",
      fn_
    );

    api.addRoutes({
      path: "/{proxy+}",
      methods: [apigw.HttpMethod.ANY],
      integration,
    });

    // Outputs
    new cdk.CfnOutput(this, "ApiUrl", {
      value: api.url ?? "unknown",
      description: "API Gateway endpoint URL",
    });

    new cdk.CfnOutput(this, "FunctionName", {
      value: fn_.functionName,
      description: "Lambda function name",
    });
  }
}
