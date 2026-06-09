# ami-s3-archive

Archive EC2 AMIs to S3 (`STANDARD_IA` by default) using EC2 `CreateStoreImageTask`.

Two deployment options:

| Option | Path | Best for |
|--------|------|----------|
| **Rust CLI** | this directory | Laptop, CI, no AWS CLI dependency |
| **Step Functions** | [stepfunctions/](./stepfunctions/) | Scheduled/automated runs in AWS |

## Rust CLI

Standalone binary — direct AWS SigV4 over HTTPS (`reqwest` + `rustls`).

### Build

```bash
cd ami-s3-archive
./build-local.sh
# or
cargo build --release
```

Cross-target release builds run via [`.github/workflows/ami-s3-archive-release.yml`](../.github/workflows/ami-s3-archive-release.yml) on tag `ami-s3-archive/v*`.

## Usage

```bash
./target/release/ami-s3-archive \
  --ami-id ami-0123456789abcdef0 \
  --region eu-west-3 \
  --s3-uri s3://my-ami-archive-bucket \
  --create-bucket \
  --cleanup \
  --yes
```

Dry run:

```bash
./target/release/ami-s3-archive \
  --ami-id ami-0123456789abcdef0 \
  --region eu-west-3 \
  --s3-uri s3://my-ami-archive-bucket \
  --dry-run \
  --verbose
```

## Flow

```text
AMI ID
  -> resolve region / credentials
  -> optional S3 bucket create + hardening (BPA, SSE-S3)
  -> EC2 CreateStoreImageTask
  -> wait (DescribeStoreImageTasks)
  -> s3://bucket/ami-xxxx.bin
  -> copy in-place to STANDARD_IA (multipart if >= 5 GiB)
  -> optional: DeregisterImage + DeleteAssociatedSnapshots

On failure after partial progress, the CLI attempts best-effort rollback:
abort in-flight multipart uploads, delete incomplete objects created this run,
and remove an empty bucket if this run created it. Completed archive objects are kept.
```

## Credential sources

1. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / optional `AWS_SESSION_TOKEN`
2. `~/.aws/credentials` profile (`AWS_PROFILE` or `default`)
3. ECS task role
4. EC2 IMDS

## Layout

```text
src/
  main.rs          CLI entrypoint
  lib.rs           public API
  config.rs        args + validation
  credentials.rs   auth chain (INI cached)
  workflow.rs      orchestration
  aws/             SigV4 client (EC2, S3, STS)
  xml.rs           EC2 XML parsing
stepfunctions/     SAM template + Step Functions workflow (see stepfunctions/README.md)
```
