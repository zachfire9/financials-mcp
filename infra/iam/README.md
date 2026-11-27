# Deploy role IAM policies

The IAM role GitHub Actions deploys with is managed by hand in AWS, not by `template.yaml`, as in financials-api. These files are the source of truth for what that role may do. Change them in a PR first, then apply them.

GitHub never holds AWS keys. Each deploy run asks GitHub for a short-lived OIDC token that says "this is `zachfire9/financials-mcp`, branch `main`", and AWS STS trades it for credentials that last about an hour. The trust policy is what makes AWS accept that token, and only from `main`.

| File | Applies to |
|------|------------|
| `github-oidc-trust.json` | Trust policy of `financials-mcp-github-deploy-role`: only `main` of this repo can use the role |
| `github-deploy-policy.json` | Inline policy `financials-mcp-github-deploy-policy`: deploy the stack (CloudFormation, the function, the HTTP API, the function's role, its log group, SAM's artifact bucket) |
| `sam-managed-stack-policy.json` | Inline policy `financials-mcp-sam-managed-stack-policy`: manage SAM's `aws-sam-cli-managed-default` artifact-bucket stack |
| `cloudformation-template-summary-policy.json` | Inline policy `financials-mcp-cloudformation-template-summary-policy`: `GetTemplateSummary` for both stacks |

## Placeholders

Fill these in a copy before applying:

| Placeholder | File | Where to find it |
|-------------|------|------------------|
| `<aws-account-id>` | `github-oidc-trust.json` | `aws sts get-caller-identity --query Account --output text` |
| `<api-id>` | `github-deploy-policy.json` | The subdomain of the deployed `McpUrl` (`https://<api-id>.execute-api.us-east-1.amazonaws.com/mcp`), or the `McpHttpApi` resource in the `financials-mcp` stack. **Before the first deploy there is no API yet**: use `*`, deploy once, then pin it (see below) |

The other two policies have no placeholders. The policies assume `us-east-1` and the stack name `financials-mcp`. If you change `AWS_REGION` or `MCP_STACK_NAME`, change the ARNs to match.

## First-time setup

The GitHub OIDC provider (`token.actions.githubusercontent.com`) is account-wide. If financials-api already deploys from GitHub Actions, it exists. Check with `aws iam list-open-id-connect-providers`. If it doesn't exist, create it from the IAM console (Identity providers → Add provider → OpenID Connect, provider URL `https://token.actions.githubusercontent.com`, audience `sts.amazonaws.com`).

Save filled-in copies in the repo root. `/*.filled.json` is gitignored there, so they can't be committed by accident.

```powershell
$r = "financials-mcp-github-deploy-role"
aws iam create-role --role-name $r --assume-role-policy-document file://github-oidc-trust.filled.json
aws iam put-role-policy --role-name $r --policy-name financials-mcp-github-deploy-policy --policy-document file://github-deploy-policy.filled.json   # <api-id> = * for now
aws iam put-role-policy --role-name $r --policy-name financials-mcp-sam-managed-stack-policy --policy-document file://infra/iam/sam-managed-stack-policy.json
aws iam put-role-policy --role-name $r --policy-name financials-mcp-cloudformation-template-summary-policy --policy-document file://infra/iam/cloudformation-template-summary-policy.json
aws iam get-role --role-name $r --query Role.Arn --output text   # -> the AWS_DEPLOY_ROLE_ARN repository variable
```

Then run the deploy workflow once ([README: Deploying](../../README.md#deploying-to-aws)). Then pin the API:

```powershell
aws cloudformation describe-stack-resource --stack-name financials-mcp --logical-resource-id McpHttpApi --query StackResourceDetail.PhysicalResourceId --output text
# put that id in place of * in github-deploy-policy.filled.json, then:
aws iam put-role-policy --role-name $r --policy-name financials-mcp-github-deploy-policy --policy-document file://github-deploy-policy.filled.json
```

Run the deploy workflow again to confirm the pinned policy still works.

## Changing a policy later

Apply only what changed with `aws iam update-assume-role-policy` (the trust policy) or `aws iam put-role-policy` (the others, same names as above), then run the deploy workflow once.

## Notes

- **The role can't change itself.** IAM permissions cover only `financials-mcp-McpFunctionRole-*`, the role SAM creates for the function, not `financials-mcp-*`. The broader pattern would also match `financials-mcp-github-deploy-role`, letting a deploy run rewrite its own policies. Lambda and log permissions are scoped to `financials-mcp-McpFunction-*` the same way. CloudFormation names resources `<stack>-<logical id>-<random>`, which is why these patterns match.
- S3 access is limited to SAM's managed artifact bucket (`aws-sam-cli-managed-default-samclisourcebucket-*`).
- API Gateway access is limited to this stack's HTTP API and everything under it (integrations, routes, stages), plus listing and creating APIs and tagging. API Gateway permissions are HTTP methods on resource paths, not named actions.
- The API id is pinned. If CloudFormation ever replaces the API, the `McpUrl` changes: update `<api-id>` and the URL in your MCP clients.
- `logs:DescribeLogGroups` is granted on `*`. It's read-only, and listing can't be scoped to one group.
- **These policies were written from the template's resources, not yet exercised against AWS.** The first deploy is the test. If it fails with `AccessDenied`, the CloudFormation event names the missing action. Add it here in a PR.
