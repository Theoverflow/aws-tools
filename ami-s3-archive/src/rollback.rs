use crate::aws::AwsClient;
use crate::config::Config;

/// Tracks workflow mutations so failures can be rolled back best-effort.
#[derive(Debug, Clone)]
pub struct RollbackContext {
    pub bucket: String,
    pub object_key: String,
    pub dry_run: bool,
    pub created_bucket: bool,
    pub object_existed_before: bool,
    pub store_task_started_this_run: bool,
    pub archive_object_ready: bool,
    pub conversion_attempted: bool,
    pub storage_class_changed: bool,
}

impl RollbackContext {
    pub fn new(bucket: String, object_key: String, dry_run: bool) -> Self {
        Self {
            bucket,
            object_key,
            dry_run,
            created_bucket: false,
            object_existed_before: false,
            store_task_started_this_run: false,
            archive_object_ready: false,
            conversion_attempted: false,
            storage_class_changed: false,
        }
    }

    pub fn set_object_key(&mut self, object_key: String) {
        self.object_key = object_key;
    }

    pub fn mark_archive_ready(&mut self) {
        self.archive_object_ready = true;
    }

    pub fn should_delete_incomplete_object(&self) -> bool {
        self.store_task_started_this_run
            && !self.object_existed_before
            && !self.archive_object_ready
    }

    pub fn should_abort_multipart_uploads(&self) -> bool {
        self.conversion_attempted && !self.storage_class_changed
    }

    pub fn should_delete_created_bucket(&self) -> bool {
        self.created_bucket
    }

    pub fn execute(&self, client: &AwsClient, cfg: &Config) {
        if self.dry_run || cfg.dry_run {
            client
                .log
                .info("dry-run: skipping rollback (no resources were mutated)");
            return;
        }

        client
            .log
            .warn("workflow failed; attempting best-effort rollback");

        if self.should_abort_multipart_uploads() {
            match client.s3_abort_multipart_uploads_for_key(&self.bucket, &self.object_key) {
                Ok(count) if count > 0 => client.log.info(format!(
                    "rollback: aborted {count} in-progress multipart upload(s) for s3://{}/{}",
                    self.bucket, self.object_key
                )),
                Ok(_) => {}
                Err(e) => client
                    .log
                    .warn(format!("rollback: could not abort multipart uploads: {e}")),
            }
        }

        if self.should_delete_incomplete_object() {
            match client.s3_head_object(&self.bucket, &self.object_key) {
                Ok(head) if head.exists => match client.s3_delete_object(&self.bucket, &self.object_key)
                {
                    Ok(()) => client.log.info(format!(
                        "rollback: deleted incomplete object s3://{}/{}",
                        self.bucket, self.object_key
                    )),
                    Err(e) => client.log.warn(format!(
                        "rollback: could not delete incomplete object s3://{}/{}: {e}",
                        self.bucket, self.object_key
                    )),
                },
                Ok(_) => {}
                Err(e) => client.log.warn(format!(
                    "rollback: could not inspect object s3://{}/{}: {e}",
                    self.bucket, self.object_key
                )),
            }
        }

        if self.should_delete_created_bucket() {
            match client.s3_delete_bucket(&self.bucket) {
                Ok(()) => client
                    .log
                    .info(format!("rollback: deleted empty bucket s3://{}", self.bucket)),
                Err(e) => client.log.warn(format!(
                    "rollback: could not delete bucket s3://{} (it may not be empty): {e}",
                    self.bucket
                )),
            }
        }

        if self.archive_object_ready {
            client.log.info(format!(
                "rollback: archive object remains at s3://{}/{}; re-run to resume conversion or cleanup",
                self.bucket, self.object_key
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_incomplete_object_only_when_store_started_and_not_ready() {
        let ctx = RollbackContext {
            bucket: "b".into(),
            object_key: "ami-1.bin".into(),
            dry_run: false,
            created_bucket: false,
            object_existed_before: false,
            store_task_started_this_run: true,
            archive_object_ready: false,
            conversion_attempted: false,
            storage_class_changed: false,
        };
        assert!(ctx.should_delete_incomplete_object());

        let mut ready = ctx.clone();
        ready.archive_object_ready = true;
        assert!(!ready.should_delete_incomplete_object());

        let mut existed = ctx.clone();
        existed.object_existed_before = true;
        assert!(!existed.should_delete_incomplete_object());
    }

    #[test]
    fn aborts_multipart_only_when_conversion_failed() {
        let ctx = RollbackContext {
            bucket: "b".into(),
            object_key: "k".into(),
            dry_run: false,
            created_bucket: false,
            object_existed_before: true,
            store_task_started_this_run: false,
            archive_object_ready: true,
            conversion_attempted: true,
            storage_class_changed: false,
        };
        assert!(ctx.should_abort_multipart_uploads());

        let mut done = ctx.clone();
        done.storage_class_changed = true;
        assert!(!done.should_abort_multipart_uploads());
    }
}
