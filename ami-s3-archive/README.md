# ami-s3-archive

Standalone Rust CLI to archive EC2 AMIs to S3 (`STANDARD_IA` by default) using `CreateStoreImageTask`.

No `aws` CLI, no Python — direct AWS SigV4 over HTTPS (`reqwest` + `rustls`).

## Build

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
```
