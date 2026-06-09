use crate::aws::AwsClient;
use crate::config::Config;
use crate::error::{empty_default, AnyError};
use crate::prompt::ask_yn;
use crate::rollback::RollbackContext;
use reqwest::blocking::Client;
use std::time::{Duration, Instant};

pub fn run(cfg: &Config, client: &AwsClient, deadline: Instant) -> Result<(), AnyError> {
    let account = client.get_caller_identity()?;
    client
        .log
        .info(format!("authenticated account={account}"));

    let mut bucket = cfg.bucket.clone();
    if bucket.is_empty() {
        bucket = format!("ami-archive-{}-{}", account, cfg.region);
        client
            .log
            .info(format!("no --s3-uri provided; proposed bucket=s3://{bucket}"));
        if !cfg.create_bucket && !cfg.yes {
            return Err(
                "bucket not provided. Re-run with --s3-uri s3://bucket, or allow proposed bucket creation with --create-bucket".into(),
            );
        }
    }

    let object_key = format!("{}.bin", cfg.ami_id);
    let mut rollback = RollbackContext::new(bucket.clone(), object_key.clone(), cfg.dry_run);

    let result = run_workflow(cfg, client, deadline, &mut bucket, &mut rollback);
    if result.is_err() {
        rollback.execute(client, cfg);
    }
    result
}

fn run_workflow(
    cfg: &Config,
    client: &AwsClient,
    deadline: Instant,
    bucket: &mut String,
    rollback: &mut RollbackContext,
) -> Result<(), AnyError> {
    if cfg.dry_run {
        client
            .log
            .info(format!("dry-run: would validate or create bucket s3://{bucket}"));
    } else {
        ensure_bucket(client, cfg, bucket, rollback)?;
    }

    let mut object_key = rollback.object_key.clone();
    if cfg.dry_run {
        client
            .log
            .info(format!("dry-run: would check object s3://{bucket}/{object_key}"));
        client
            .ec2_dry_run_create_store_image_task(&cfg.ami_id, bucket)?;
        client
            .log
            .info("dry-run: EC2 CreateStoreImageTask permission check passed");
    } else {
        object_key = archive_object(client, cfg, bucket, object_key, deadline, rollback)?;
    }

    client
        .log
        .info(format!("archive ready: s3://{bucket}/{object_key}"));

    if cfg.cleanup {
        maybe_cleanup(client, cfg)?;
    }

    Ok(())
}

fn ensure_bucket(
    client: &AwsClient,
    cfg: &Config,
    bucket: &str,
    rollback: &mut RollbackContext,
) -> Result<(), AnyError> {
    let exists = client.s3_bucket_exists(bucket)?;
    if !exists {
        if !cfg.create_bucket && !cfg.yes {
            return Err(format!(
                "bucket s3://{bucket} does not exist; pass --create-bucket or create it first"
            )
            .into());
        }
        if !cfg.yes
            && !ask_yn(
                &format!("Create bucket s3://{bucket} in region {}?", cfg.region),
                false,
            )?
        {
            return Err("bucket creation declined".into());
        }
        client.s3_create_bucket(bucket)?;
        rollback.created_bucket = true;
        client.log.info(format!("created bucket s3://{bucket}"));
    }
    client.s3_put_public_access_block(bucket)?;
    client
        .log
        .info(format!("ensured S3 Block Public Access on s3://{bucket}"));
    client.s3_put_bucket_encryption(bucket)?;
    client
        .log
        .info(format!("ensured bucket default SSE-S3 encryption on s3://{bucket}"));
    Ok(())
}

fn archive_object(
    client: &AwsClient,
    cfg: &Config,
    bucket: &str,
    mut object_key: String,
    deadline: Instant,
    rollback: &mut RollbackContext,
) -> Result<String, AnyError> {
    let mut head = client.s3_head_object(bucket, &object_key)?;
    rollback.object_existed_before = head.exists;
    if head.exists {
        client.log.info(format!(
            "object already exists: s3://{bucket}/{object_key} storage_class={} size={}",
            empty_default(&head.storage_class, "STANDARD"),
            head.content_length
        ));
    } else {
        let (key, started_new) =
            start_or_resume_store_task(client, cfg, bucket, object_key, deadline)?;
        rollback.store_task_started_this_run = started_new;
        object_key = key;
        rollback.set_object_key(object_key.clone());
    }

    head = client.s3_head_object(bucket, &object_key)?;
    if !head.exists {
        return Err(format!(
            "expected object not found after completed store task: s3://{bucket}/{object_key}"
        )
        .into());
    }
    rollback.mark_archive_ready();

    let current_class = empty_default(&head.storage_class, "STANDARD").to_string();
    if current_class != cfg.storage_class {
        client.log.info(format!(
            "transitioning object storage class: s3://{bucket}/{object_key} {current_class} -> {}",
            cfg.storage_class
        ));
        rollback.conversion_attempted = true;
        client.s3_copy_object_to_storage_class(
            bucket,
            &object_key,
            &cfg.storage_class,
            head.content_length,
        )?;
        rollback.storage_class_changed = true;
        client
            .log
            .info(format!("object storage class updated to {}", cfg.storage_class));
    } else {
        client.log.info(format!(
            "object already in requested storage class {}",
            cfg.storage_class
        ));
    }

    Ok(object_key)
}

fn start_or_resume_store_task(
    client: &AwsClient,
    cfg: &Config,
    bucket: &str,
    mut object_key: String,
    deadline: Instant,
) -> Result<(String, bool), AnyError> {
    let mut started_new = false;
    let task = client.ec2_latest_store_image_task(&cfg.ami_id)?;
    if !task.image_id.is_empty()
        && task.bucket.eq_ignore_ascii_case(bucket)
        && task.state.eq_ignore_ascii_case("Completed")
        && !task.object_key.is_empty()
    {
        object_key = task.object_key;
        client.log.info(format!(
            "found completed store image task object=s3://{bucket}/{object_key}"
        ));
    } else if !task.image_id.is_empty()
        && task.bucket.eq_ignore_ascii_case(bucket)
        && task.state.eq_ignore_ascii_case("InProgress")
    {
        if !task.object_key.is_empty() {
            object_key = task.object_key;
        }
        client.log.info(format!(
            "found in-progress store image task image={} progress={}%",
            cfg.ami_id,
            empty_default(&task.progress, "?")
        ));
    } else {
        object_key = client.ec2_create_store_image_task(&cfg.ami_id, bucket)?;
        started_new = true;
        client.log.info(format!(
            "created store image task object=s3://{bucket}/{object_key}"
        ));
    }

    if cfg.wait {
        let final_task =
            client.ec2_wait_store_image_task(&cfg.ami_id, bucket, cfg.poll_seconds, deadline)?;
        if !final_task.object_key.is_empty() {
            object_key = final_task.object_key;
        }
        client.log.info(format!(
            "store image task completed object=s3://{bucket}/{object_key}"
        ));
    } else {
        client.log.info(
            "not waiting; run again later to finalize STANDARD_IA transition and cleanup",
        );
    }

    Ok((object_key, started_new))
}

fn maybe_cleanup(client: &AwsClient, cfg: &Config) -> Result<(), AnyError> {
    if let Ok(snapshots) = client.ec2_describe_image_snapshot_ids(&cfg.ami_id) {
        if !snapshots.is_empty() {
            client.log.info(format!(
                "AMI associated EBS snapshots: {}",
                snapshots.join(",")
            ));
        }
    }
    if cfg.dry_run {
        client.ec2_dry_run_deregister_image(&cfg.ami_id)?;
        client.log.info(format!(
            "dry-run: would deregister AMI {} with DeleteAssociatedSnapshots=true",
            cfg.ami_id
        ));
        return Ok(());
    }
    if !cfg.yes
        && !ask_yn(
            &format!(
                "Deregister {} and delete associated EBS snapshots?",
                cfg.ami_id
            ),
            false,
        )?
    {
        client.log.info("cleanup skipped");
        return Ok(());
    }
    let results = client.ec2_deregister_image_delete_snapshots(&cfg.ami_id)?;
    client.log.info("cleanup completed: deregister requested");
    for (snap, code) in results {
        if code.eq_ignore_ascii_case("success") || code.is_empty() {
            client.log.info(format!(
                "snapshot cleanup: {} {}",
                snap,
                empty_default(&code, "requested")
            ));
        } else {
            client
                .log
                .warn(format!("snapshot cleanup: {} return_code={code}", snap));
        }
    }
    Ok(())
}

pub fn build_http_client() -> Result<Client, AnyError> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent(crate::constants::USER_AGENT)
        .build()?)
}
