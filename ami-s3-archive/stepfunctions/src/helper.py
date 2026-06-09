import json
import math
import os
import re
import urllib.parse
from typing import Any, Dict, Optional

import boto3
from botocore.exceptions import ClientError

FIVE_GIB = 5 * 1024 ** 3
DEFAULT_PART_SIZE = 512 * 1024 ** 2  # 512 MiB; adjusted upward when needed.
MAX_PARTS = 9500  # keep margin below S3 hard limit of 10,000 parts.
AMI_ID_RE = re.compile(r"^ami-[0-9a-fA-F]{8,17}$")


def _bool(value: Any, default: bool = False) -> bool:
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "y", "on"}
    return bool(value)


def _head_object(s3, bucket: str, key: str) -> Optional[Dict[str, Any]]:
    try:
        return s3.head_object(Bucket=bucket, Key=key)
    except ClientError as exc:
        code = exc.response.get("Error", {}).get("Code", "")
        status = exc.response.get("ResponseMetadata", {}).get("HTTPStatusCode")
        if code in {"404", "NoSuchKey", "NotFound"} or status == 404:
            return None
        raise


def _bucket_exists_or_owned(s3, bucket: str) -> bool:
    try:
        s3.head_bucket(Bucket=bucket)
        return True
    except ClientError as exc:
        code = exc.response.get("Error", {}).get("Code", "")
        status = exc.response.get("ResponseMetadata", {}).get("HTTPStatusCode")
        if code in {"404", "NoSuchBucket", "NotFound"} or status == 404:
            return False
        # 403 can mean the bucket exists but is not owned or not accessible.
        raise


def _create_bucket_if_needed(s3, bucket: str, region: str) -> None:
    if _bucket_exists_or_owned(s3, bucket):
        return

    if region == "us-east-1":
        s3.create_bucket(Bucket=bucket)
    else:
        s3.create_bucket(
            Bucket=bucket,
            CreateBucketConfiguration={"LocationConstraint": region},
        )

    waiter = s3.get_waiter("bucket_exists")
    waiter.wait(Bucket=bucket)


def _secure_bucket_defaults(s3, bucket: str) -> None:
    s3.put_public_access_block(
        Bucket=bucket,
        PublicAccessBlockConfiguration={
            "BlockPublicAcls": True,
            "IgnorePublicAcls": True,
            "BlockPublicPolicy": True,
            "RestrictPublicBuckets": True,
        },
    )
    s3.put_bucket_encryption(
        Bucket=bucket,
        ServerSideEncryptionConfiguration={
            "Rules": [
                {
                    "ApplyServerSideEncryptionByDefault": {
                        "SSEAlgorithm": "AES256",
                    },
                    "BucketKeyEnabled": True,
                }
            ]
        },
    )


def _region() -> str:
    return os.environ.get("AWS_REGION") or os.environ.get("AWS_DEFAULT_REGION") or "us-east-1"


def prepare(event: Dict[str, Any]) -> Dict[str, Any]:
    user_input = event.get("input", event)
    region = _region()
    ami_id = user_input.get("amiId") or user_input.get("ImageId")
    if not ami_id or not AMI_ID_RE.match(ami_id):
        raise ValueError("Input must contain a valid amiId such as ami-0123456789abcdef0")

    sts = boto3.client("sts", region_name=region)
    s3 = boto3.client("s3", region_name=region)
    account_id = sts.get_caller_identity()["Account"]

    bucket = user_input.get("bucketName")
    if not bucket:
        if not _bool(user_input.get("createBucket"), False):
            raise ValueError("Provide bucketName or set createBucket=true")
        bucket = f"ami-archive-{account_id}-{region}"

    storage_class = user_input.get("storageClass", "STANDARD_IA")
    cleanup = _bool(user_input.get("cleanupOriginalAmi"), False)
    delete_snapshots = _bool(user_input.get("deleteAssociatedSnapshots"), True)
    poll_seconds = int(user_input.get("pollSeconds", 60))
    if poll_seconds < 10:
        poll_seconds = 10

    if _bool(user_input.get("createBucket"), False):
        _create_bucket_if_needed(s3, bucket, region)
        _secure_bucket_defaults(s3, bucket)

    object_key = user_input.get("objectKey") or f"{ami_id}.bin"
    head = _head_object(s3, bucket, object_key)
    object_exists = head is not None
    current_storage_class = "STANDARD" if not head else head.get("StorageClass", "STANDARD")
    already_in_target = bool(head and current_storage_class == storage_class)

    return {
        "amiId": ami_id,
        "bucketName": bucket,
        "objectKey": object_key,
        "storageClass": storage_class,
        "region": region,
        "accountId": account_id,
        "cleanupOriginalAmi": cleanup,
        "deleteAssociatedSnapshots": delete_snapshots,
        "pollSeconds": poll_seconds,
        "objectExists": object_exists,
        "currentStorageClass": current_storage_class,
        "alreadyInTargetStorageClass": already_in_target,
    }


def _copy_small_object(s3, bucket: str, key: str, storage_class: str) -> Dict[str, Any]:
    s3.copy_object(
        Bucket=bucket,
        Key=key,
        CopySource={"Bucket": bucket, "Key": key},
        StorageClass=storage_class,
        MetadataDirective="COPY",
        TaggingDirective="COPY",
        ServerSideEncryption="AES256",
    )
    return {"method": "copy_object"}


def _multipart_copy_same_key(s3, bucket: str, key: str, size: int, storage_class: str) -> Dict[str, Any]:
    part_size = max(DEFAULT_PART_SIZE, math.ceil(size / MAX_PARTS))
    create = s3.create_multipart_upload(
        Bucket=bucket,
        Key=key,
        StorageClass=storage_class,
        ServerSideEncryption="AES256",
    )
    upload_id = create["UploadId"]
    parts = []
    copy_source = {"Bucket": bucket, "Key": key}

    try:
        part_number = 1
        offset = 0
        while offset < size:
            end = min(offset + part_size - 1, size - 1)
            resp = s3.upload_part_copy(
                Bucket=bucket,
                Key=key,
                UploadId=upload_id,
                PartNumber=part_number,
                CopySource=copy_source,
                CopySourceRange=f"bytes={offset}-{end}",
            )
            parts.append({"ETag": resp["CopyPartResult"]["ETag"], "PartNumber": part_number})
            offset = end + 1
            part_number += 1

        s3.complete_multipart_upload(
            Bucket=bucket,
            Key=key,
            UploadId=upload_id,
            MultipartUpload={"Parts": parts},
        )
        return {"method": "multipart_copy", "parts": len(parts), "partSize": part_size}
    except Exception:
        s3.abort_multipart_upload(Bucket=bucket, Key=key, UploadId=upload_id)
        raise


def convert(event: Dict[str, Any]) -> Dict[str, Any]:
    region = _region()
    s3 = boto3.client("s3", region_name=region)

    bucket = event["bucketName"]
    key = event["objectKey"]
    storage_class = event.get("storageClass", "STANDARD_IA")

    head = _head_object(s3, bucket, key)
    if not head:
        raise FileNotFoundError(f"s3://{bucket}/{key} does not exist yet")

    current_storage_class = head.get("StorageClass", "STANDARD")
    size = int(head["ContentLength"])
    if current_storage_class == storage_class:
        return {
            "bucketName": bucket,
            "objectKey": key,
            "storageClass": current_storage_class,
            "sizeBytes": size,
            "changed": False,
            "method": "none",
        }

    if size < FIVE_GIB:
        details = _copy_small_object(s3, bucket, key, storage_class)
    else:
        details = _multipart_copy_same_key(s3, bucket, key, size, storage_class)

    final_head = s3.head_object(Bucket=bucket, Key=key)
    return {
        "bucketName": bucket,
        "objectKey": key,
        "storageClass": final_head.get("StorageClass", "STANDARD"),
        "sizeBytes": int(final_head["ContentLength"]),
        "changed": True,
        **details,
    }


def handler(event, context):
    op = event.get("op")
    if op == "prepare":
        return prepare(event)
    if op == "convert":
        return convert(event)
    raise ValueError("Unknown op. Expected op=prepare or op=convert")
