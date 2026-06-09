# AMI archive to S3 Standard-IA (Step Functions)

Serverless workflow that archives an EC2 AMI to S3, converts the resulting `.bin` object to `STANDARD_IA`, and optionally deregisters the source AMI.

This is the **orchestrated** deployment option for [ami-s3-archive](../README.md). The Rust CLI in the parent directory performs the same workflow locally; this SAM stack runs it as a Step Functions state machine with a small helper Lambda.

## What gets deployed

| Resource | Purpose |
|----------|---------|
| **Step Functions state machine** | Orchestrates EC2 `CreateStoreImageTask`, polling, S3 conversion, optional cleanup |
| **Helper Lambda** (`helper.py`) | Bucket prep, S3 copy/multipart copy, rollback |
| **IAM roles** | Least-privilege permissions for EC2 AMI Store, EBS Direct API, S3, and Lambda invoke |

## Architecture

```text
Input (amiId, bucket, options)
  -> PrepareArchive (Lambda: resolve bucket, head object)
  -> DescribeExistingStoreTask (EC2 SDK)
  -> [CreateStoreImageTask if needed]
  -> Poll until Completed
  -> ConvertObjectToStorageClass (Lambda: CopyObject or multipart copy)
  -> [Optional DeregisterImage]
  -> Success

On failure after partial progress, the state machine invokes `RollbackResources` (same policy as the Rust CLI).
```

## Idempotency

Re-running an execution with the same input is safe:

| Existing state | Behavior |
|----------------|----------|
| Object already in target storage class | Skip store + conversion → optional cleanup |
| Object exists but wrong storage class | Skip store → convert only |
| EC2 store task `Completed` | Skip store → convert |
| EC2 store task `InProgress` | Resume polling (no new store task) |
| Nothing started yet | Create store task |

The `convert` helper is also idempotent: it no-ops when the object is already in the requested storage class.

## Rollback on failure

If the workflow fails after mutating resources, the `RollbackResources` state invokes the helper with `op=rollback`:

| Condition | Rollback action |
|-----------|-----------------|
| Storage-class conversion failed mid-way | Abort in-progress multipart uploads |
| Store task started this run, archive never verified | Delete incomplete S3 object |
| Bucket created this run | Delete bucket (only if empty) |
| Archive completed | **Preserve** object — re-run execution to resume |

Rollback actions are recorded in `$.rollback.result.actions` in the execution history.

Cleanup failures (`CleanupFailed`) do **not** roll back a successful archive.

## Prerequisites

- AWS account with permissions to deploy CloudFormation/SAM stacks (IAM, Lambda, Step Functions)
- [AWS CLI v2](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html) configured (`aws configure` or SSO)
- [AWS SAM CLI](https://docs.aws.amazon.com/serverless-application-model/latest/developerguide/install-sam-cli.html)
- The AMI must exist in the **same region** you deploy to

Deployer IAM needs at minimum: `cloudformation:*`, `iam:CreateRole`, `iam:AttachRolePolicy`, `lambda:*`, `states:*`, and `s3:CreateBucket` (if using `createBucket=true` at runtime).

## Deploy

From this directory (`ami-s3-archive/stepfunctions/`):

```bash
cd ami-s3-archive/stepfunctions

# Validate and build
sam build

# First deploy — interactive prompts for stack name, region, etc.
sam deploy --guided
```

On subsequent deploys, after `samconfig.toml` is written:

```bash
sam build && sam deploy
```

Edit `samconfig.toml` to change the default region or stack name before deploying. The sample config targets `eu-west-3`; replace with your region.

### Stack outputs

After deploy, note the outputs:

```bash
aws cloudformation describe-stacks \
  --stack-name ami-s3-archive-stepfunctions \
  --query 'Stacks[0].Outputs'
```

| Output | Use |
|--------|-----|
| `StateMachineArn` | Start executions (see below) |
| `HelperFunctionArn` | Debugging Lambda logs |

## Run an archive

Replace `ami-xxxxxxxxxxxxxxxxx` with a real AMI ID in your deploy region.

### Option A — create a default archive bucket

Uses bucket name `ami-archive-{accountId}-{region}` and applies public-access block + SSE-S3 encryption.

```bash
STATE_MACHINE_ARN=$(aws cloudformation describe-stacks \
  --stack-name ami-s3-archive-stepfunctions \
  --query 'Stacks[0].Outputs[?OutputKey==`StateMachineArn`].OutputValue' \
  --output text)

aws stepfunctions start-execution \
  --state-machine-arn "$STATE_MACHINE_ARN" \
  --name "archive-$(date +%Y%m%d-%H%M%S)" \
  --input file://sample-input.json
```

Edit `sample-input.json` and set your `amiId`:

```json
{
  "amiId": "ami-0123456789abcdef0",
  "createBucket": true,
  "storageClass": "STANDARD_IA",
  "cleanupOriginalAmi": false,
  "deleteAssociatedSnapshots": true,
  "pollSeconds": 60
}
```

### Option B — use an existing S3 bucket

The bucket must already exist and the state machine role must be allowed to write to it (the template grants `s3:*` on `*` for the store task; tighten in production).

```bash
aws stepfunctions start-execution \
  --state-machine-arn "$STATE_MACHINE_ARN" \
  --name "archive-existing-bucket-$(date +%Y%m%d-%H%M%S)" \
  --input file://sample-input-existing-bucket.json
```

### Monitor execution

Console: **Step Functions → State machines → ami-archive-to-s3-ia → Executions**

CLI:

```bash
EXEC_ARN="<execution-arn-from-start-execution>"

aws stepfunctions describe-execution --execution-arn "$EXEC_ARN"

aws stepfunctions get-execution-history \
  --execution-arn "$EXEC_ARN" \
  --max-results 20 \
  --reverse-order
```

Large AMI exports can take 30–90+ minutes. The state machine polls every `pollSeconds` (default 60, minimum 10).

## Input parameters

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `amiId` | yes | — | Source AMI ID (`ami-…`) |
| `createBucket` | no | `false` | Create `ami-archive-{account}-{region}` if `bucketName` omitted |
| `bucketName` | no* | auto | Target S3 bucket (*required if `createBucket` is false) |
| `objectKey` | no | `{amiId}.bin` | S3 object key for the archive |
| `storageClass` | no | `STANDARD_IA` | Target storage class after EC2 store completes |
| `cleanupOriginalAmi` | no | `false` | Deregister source AMI when done |
| `deleteAssociatedSnapshots` | no | `true` | Passed to `DeregisterImage` when cleanup is enabled |
| `pollSeconds` | no | `60` | Wait between `DescribeStoreImageTasks` polls (min 10) |

## Cleanup behavior

There is no interactive confirmation in Step Functions. The safe default is **no cleanup** (`cleanupOriginalAmi: false`).

To remove the source AMI and associated snapshots after a successful archive:

```json
{
  "amiId": "ami-0123456789abcdef0",
  "bucketName": "my-ami-archive-bucket",
  "cleanupOriginalAmi": true,
  "deleteAssociatedSnapshots": true
}
```

Snapshots shared across multiple AMIs are **not** deleted by EC2 even when `deleteAssociatedSnapshots` is true.

## Why a helper Lambda exists

Step Functions can call EC2 and S3 through AWS SDK integrations, but S3 `CopyObject` is limited to **5 GiB** per request. AMI `.bin` archives can be larger, so the helper Lambda performs either:

- single-request `CopyObject` for smaller objects, or
- multipart in-place copy for larger objects.

The Lambda also creates and hardens the archive bucket when `createBucket=true`.

## Restore later

Use EC2 `CreateRestoreImageTask` against the S3 object (e.g. `s3://bucket/ami-0123456789abcdef0.bin`). The restored AMI receives a new AMI ID.

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `StoreImageTaskFailed` | `aws ec2 describe-store-image-tasks --image-ids ami-…` and CloudTrail |
| Lambda timeout on convert | Increase `HelperTimeoutSeconds` (max 900) in `template.yaml` / deploy params |
| Access denied on bucket | Bucket policy must allow the state machine role and EC2 store task principal |
| `WorkflowFailed` | Inspect `$.rollback.result.actions`; fix root cause and re-run |
| `CleanupFailed` | Archive succeeded; deregister manually or re-run with `cleanupOriginalAmi=true` |
| `ResourceExistenceCheck` failed on deploy | Lambda/state machine name already used by another stack — update the existing stack or use different `HelperFunctionName` / `StateMachineName` |
| Stack stuck in `REVIEW_IN_PROGRESS` | `aws cloudformation delete-stack --stack-name <name>` then redeploy |

Helper Lambda logs:

```bash
aws logs tail /aws/lambda/ami-archive-helper --follow
```

## Files

```text
template.yaml                          SAM template (Lambda + state machine + IAM)
statemachine/archive_ami_to_s3_ia.asl.json
src/helper.py                          prepare, convert, and rollback operations
sample-input.json                      Example: auto-create bucket
sample-input-existing-bucket.json      Example: existing bucket
samconfig.toml                         SAM CLI deploy defaults (edit region)
```

## Related

- [Rust CLI](../README.md) — same workflow, no AWS CLI/SAM, runs from your laptop or CI
