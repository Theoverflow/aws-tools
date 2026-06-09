use crate::aws::endpoints::{
    ec2_endpoint, escape_s3_key, form_body, s3_endpoint, sts_endpoint,
};
use crate::aws::retry::{is_s3_retryable, s3_backoff_delay, S3_MAX_ATTEMPTS};
use crate::aws::sign::sign_v4;
use crate::constants::{
    DEFAULT_PART_SIZE_BYTES, EC2_API_VERSION, MAX_SINGLE_COPY_BYTES, USER_AGENT,
};
use crate::error::{summarize, AnyError, AwsError};
use crate::log::Logger;
use crate::types::{Credentials, MultipartPart, ObjectHead, StoreImageTask};
use crate::xml::{
    parse_delete_snapshot_results, parse_store_image_tasks, xml_escape, xml_tag_first,
    xml_tags_all,
};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::BTreeSet;
use std::thread;
use std::time::{Duration, Instant};

pub struct HttpResult {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub struct AwsClient {
    pub region: String,
    pub creds: Credentials,
    http: Client,
    pub log: Logger,
}

impl AwsClient {
    pub fn new(region: String, creds: Credentials, http: Client, log: Logger) -> Self {
        Self {
            region,
            creds,
            http,
            log,
        }
    }

    pub fn signed_request(
        &self,
        service: &str,
        method: &str,
        endpoint: &str,
        path: &str,
        raw_query: &str,
        headers: Vec<(&str, String)>,
        body: Vec<u8>,
    ) -> Result<HttpResult, AwsError> {
        let url = if raw_query.is_empty() {
            format!("{endpoint}{path}")
        } else {
            format!("{endpoint}{path}?{raw_query}")
        };
        let signed = sign_v4(method, &url, service, &self.region, &self.creds, headers, &body);
        self.log.debug(format!("{method} {url}"));

        let method_obj: reqwest::Method = method.parse().map_err(|e| AwsError {
            service: service.to_string(),
            method: method.to_string(),
            status: 0,
            body: format!("invalid method: {e}"),
        })?;
        let mut req = self.http.request(method_obj, &url);
        for (k, v) in &signed {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| AwsError {
                service: service.to_string(),
                method: method.to_string(),
                status: 0,
                body: format!("invalid header name {k}: {e}"),
            })?;
            let value = HeaderValue::from_str(v).map_err(|e| AwsError {
                service: service.to_string(),
                method: method.to_string(),
                status: 0,
                body: format!("invalid header value for {k}: {e}"),
            })?;
            req = req.header(name, value);
        }
        let resp = req.body(body).send().map_err(|e| AwsError {
            service: service.to_string(),
            method: method.to_string(),
            status: 0,
            body: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let bytes = resp
            .bytes()
            .map_err(|e| AwsError {
                service: service.to_string(),
                method: method.to_string(),
                status,
                body: e.to_string(),
            })?
            .to_vec();
        if !(200..=299).contains(&status) {
            return Err(AwsError {
                service: service.to_string(),
                method: method.to_string(),
                status,
                body: String::from_utf8_lossy(&bytes).to_string(),
            });
        }
        Ok(HttpResult {
            status,
            headers,
            body: bytes,
        })
    }

    fn s3_signed_request(
        &self,
        method: &str,
        endpoint: &str,
        path: &str,
        raw_query: &str,
        headers: Vec<(&str, String)>,
        body: Vec<u8>,
    ) -> Result<HttpResult, AwsError> {
        let mut attempt = 0u32;
        loop {
            match self.signed_request(
                "s3",
                method,
                endpoint,
                path,
                raw_query,
                headers.clone(),
                body.clone(),
            ) {
                Ok(result) => return Ok(result),
                Err(err) if is_s3_retryable(&err) && attempt + 1 < S3_MAX_ATTEMPTS => {
                    let delay = s3_backoff_delay(attempt);
                    self.log.debug(format!(
                        "S3 {method} retry {} after {}ms (HTTP {})",
                        attempt + 1,
                        delay.as_millis(),
                        err.status
                    ));
                    thread::sleep(delay);
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub fn get_caller_identity(&self) -> Result<String, AnyError> {
        let body = form_body(&[("Action", "GetCallerIdentity"), ("Version", "2011-06-15")]);
        let r = self.signed_request(
            "sts",
            "POST",
            &sts_endpoint(&self.region),
            "/",
            "",
            vec![(
                "content-type",
                "application/x-www-form-urlencoded; charset=utf-8".to_string(),
            )],
            body.into_bytes(),
        )?;
        let s = String::from_utf8_lossy(&r.body);
        let account = xml_tag_first(&s, &["Account"]).unwrap_or_default();
        if account.is_empty() {
            return Err("empty account in STS response".into());
        }
        Ok(account)
    }

    fn ec2_query(&self, mut params: Vec<(&str, String)>) -> Result<String, AnyError> {
        params.push(("Version", EC2_API_VERSION.to_string()));
        let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let body = form_body(&pairs);
        let r = self.signed_request(
            "ec2",
            "POST",
            &ec2_endpoint(&self.region),
            "/",
            "",
            vec![(
                "content-type",
                "application/x-www-form-urlencoded; charset=utf-8".to_string(),
            )],
            body.into_bytes(),
        )?;
        Ok(String::from_utf8_lossy(&r.body).to_string())
    }

    pub fn ec2_dry_run_create_store_image_task(
        &self,
        image_id: &str,
        bucket: &str,
    ) -> Result<(), AnyError> {
        let p = vec![
            ("Action", "CreateStoreImageTask".to_string()),
            ("ImageId", image_id.to_string()),
            ("Bucket", bucket.to_string()),
            ("DryRun", "true".to_string()),
        ];
        match self.ec2_query(p) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("DryRunOperation") => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn ec2_create_store_image_task(
        &self,
        image_id: &str,
        bucket: &str,
    ) -> Result<String, AnyError> {
        let p = vec![
            ("Action", "CreateStoreImageTask".to_string()),
            ("ImageId", image_id.to_string()),
            ("Bucket", bucket.to_string()),
            ("S3ObjectTag.1.Key", "ArchivedBy".to_string()),
            ("S3ObjectTag.1.Value", USER_AGENT.to_string()),
            ("S3ObjectTag.2.Key", "SourceAmiId".to_string()),
            ("S3ObjectTag.2.Value", image_id.to_string()),
        ];
        let body = self.ec2_query(p)?;
        let key = xml_tag_first(&body, &["objectKey", "s3ObjectKey", "s3objectKey"])
            .unwrap_or_default();
        if key.is_empty() {
            return Err(format!(
                "objectKey not found in CreateStoreImageTask response: {}",
                summarize(&body)
            )
            .into());
        }
        Ok(key)
    }

    pub fn ec2_latest_store_image_task(&self, image_id: &str) -> Result<StoreImageTask, AnyError> {
        let p = vec![
            ("Action", "DescribeStoreImageTasks".to_string()),
            ("ImageId.1", image_id.to_string()),
        ];
        let body = self.ec2_query(p)?;
        let mut tasks = parse_store_image_tasks(&body);
        Ok(tasks.pop().unwrap_or_default())
    }

    pub fn ec2_wait_store_image_task(
        &self,
        image_id: &str,
        bucket: &str,
        poll_seconds: u64,
        deadline: Instant,
    ) -> Result<StoreImageTask, AnyError> {
        loop {
            if Instant::now() > deadline {
                return Err("timeout while waiting for EC2 store image task".into());
            }
            let task = self.ec2_latest_store_image_task(image_id)?;
            if task.image_id.is_empty() {
                self.log
                    .warn(format!("store image task not visible yet for {image_id}"));
            } else {
                self.log.info(format!(
                    "store task state={} progress={}% bucket={} object={}",
                    task.state,
                    if task.progress.is_empty() {
                        "?".to_string()
                    } else {
                        task.progress.clone()
                    },
                    task.bucket,
                    task.object_key
                ));
                if task.state.eq_ignore_ascii_case("Completed") {
                    return Ok(task);
                }
                if task.state.eq_ignore_ascii_case("Failed") {
                    return Err(format!("store image task failed for {image_id}").into());
                }
                if !bucket.is_empty()
                    && !task.bucket.is_empty()
                    && !task.bucket.eq_ignore_ascii_case(bucket)
                {
                    self.log.warn(format!(
                        "task bucket differs from requested bucket: task={} requested={bucket}",
                        task.bucket
                    ));
                }
            }
            thread::sleep(Duration::from_secs(poll_seconds));
        }
    }

    pub fn ec2_dry_run_deregister_image(&self, image_id: &str) -> Result<(), AnyError> {
        let p = vec![
            ("Action", "DeregisterImage".to_string()),
            ("ImageId", image_id.to_string()),
            ("DeleteAssociatedSnapshots", "true".to_string()),
            ("DryRun", "true".to_string()),
        ];
        match self.ec2_query(p) {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().contains("DryRunOperation") => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn ec2_deregister_image_delete_snapshots(
        &self,
        image_id: &str,
    ) -> Result<Vec<(String, String)>, AnyError> {
        let p = vec![
            ("Action", "DeregisterImage".to_string()),
            ("ImageId", image_id.to_string()),
            ("DeleteAssociatedSnapshots", "true".to_string()),
        ];
        let body = self.ec2_query(p)?;
        Ok(parse_delete_snapshot_results(&body))
    }

    pub fn ec2_describe_image_snapshot_ids(&self, image_id: &str) -> Result<Vec<String>, AnyError> {
        let p = vec![
            ("Action", "DescribeImages".to_string()),
            ("ImageId.1", image_id.to_string()),
        ];
        let body = self.ec2_query(p)?;
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for snap in xml_tags_all(&body, "snapshotId") {
            if snap.starts_with("snap-") && seen.insert(snap.clone()) {
                out.push(snap);
            }
        }
        Ok(out)
    }

    pub fn s3_bucket_exists(&self, bucket: &str) -> Result<bool, AnyError> {
        match self.s3_signed_request(
            "HEAD",
            &s3_endpoint(bucket, &self.region),
            "/",
            "",
            vec![],
            vec![],
        ) {
            Ok(_) => Ok(true),
            Err(e) if e.status == 404 => Ok(false),
            Err(e) if e.status == 301 => {
                Err("bucket exists in another region or endpoint mismatch".into())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn s3_create_bucket(&self, bucket: &str) -> Result<(), AnyError> {
        let body = if self.region == "us-east-1" {
            String::new()
        } else {
            format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                xml_escape(&self.region)
            )
        };
        self.s3_signed_request(
            "PUT",
            &s3_endpoint(bucket, &self.region),
            "/",
            "",
            vec![("content-type", "application/xml".to_string())],
            body.into_bytes(),
        )?;
        Ok(())
    }

    pub fn s3_put_public_access_block(&self, bucket: &str) -> Result<(), AnyError> {
        let body = "<PublicAccessBlockConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><BlockPublicAcls>true</BlockPublicAcls><IgnorePublicAcls>true</IgnorePublicAcls><BlockPublicPolicy>true</BlockPublicPolicy><RestrictPublicBuckets>true</RestrictPublicBuckets></PublicAccessBlockConfiguration>";
        self.s3_signed_request(
            "PUT",
            &s3_endpoint(bucket, &self.region),
            "/",
            "publicAccessBlock",
            vec![("content-type", "application/xml".to_string())],
            body.as_bytes().to_vec(),
        )?;
        Ok(())
    }

    pub fn s3_put_bucket_encryption(&self, bucket: &str) -> Result<(), AnyError> {
        let body = "<ServerSideEncryptionConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>AES256</SSEAlgorithm></ApplyServerSideEncryptionByDefault></Rule></ServerSideEncryptionConfiguration>";
        self.s3_signed_request(
            "PUT",
            &s3_endpoint(bucket, &self.region),
            "/",
            "encryption",
            vec![("content-type", "application/xml".to_string())],
            body.as_bytes().to_vec(),
        )?;
        Ok(())
    }

    pub fn s3_head_object(&self, bucket: &str, key: &str) -> Result<ObjectHead, AnyError> {
        let path = format!("/{}", escape_s3_key(key));
        match self.s3_signed_request(
            "HEAD",
            &s3_endpoint(bucket, &self.region),
            &path,
            "",
            vec![],
            vec![],
        ) {
            Ok(r) => {
                let storage_class = r
                    .headers
                    .get("x-amz-storage-class")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let content_length = r
                    .headers
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                Ok(ObjectHead {
                    exists: true,
                    storage_class,
                    content_length,
                })
            }
            Err(e) if e.status == 404 => Ok(ObjectHead::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn s3_copy_object_to_storage_class(
        &self,
        bucket: &str,
        key: &str,
        storage_class: &str,
        size: i64,
    ) -> Result<(), AnyError> {
        if size > 0 && size >= MAX_SINGLE_COPY_BYTES {
            return self
                .s3_multipart_copy_object_in_place(bucket, key, storage_class, size);
        }
        self.s3_copy_object_in_place(bucket, key, storage_class)
    }

    fn s3_copy_object_in_place(
        &self,
        bucket: &str,
        key: &str,
        storage_class: &str,
    ) -> Result<(), AnyError> {
        let path = format!("/{}", escape_s3_key(key));
        let copy_source = format!("/{}/{}", bucket, escape_s3_key(key));
        self.s3_signed_request(
            "PUT",
            &s3_endpoint(bucket, &self.region),
            &path,
            "",
            vec![
                ("x-amz-copy-source", copy_source),
                ("x-amz-metadata-directive", "COPY".to_string()),
                ("x-amz-tagging-directive", "COPY".to_string()),
                ("x-amz-storage-class", storage_class.to_string()),
            ],
            vec![],
        )?;
        Ok(())
    }

    fn s3_multipart_copy_object_in_place(
        &self,
        bucket: &str,
        key: &str,
        storage_class: &str,
        size: i64,
    ) -> Result<(), AnyError> {
        let upload_id = self.s3_create_multipart_upload(bucket, key, storage_class)?;
        let result =
            self.s3_multipart_copy_object_in_place_inner(bucket, key, size, &upload_id);
        if result.is_err() {
            let _ = self.s3_abort_multipart_upload(bucket, key, &upload_id);
        }
        result
    }

    fn s3_multipart_copy_object_in_place_inner(
        &self,
        bucket: &str,
        key: &str,
        size: i64,
        upload_id: &str,
    ) -> Result<(), AnyError> {
        let mut part_size = DEFAULT_PART_SIZE_BYTES;
        if (size + part_size - 1) / part_size > 10_000 {
            part_size = (size + 9_999) / 10_000;
        }
        let parts_count = ((size + part_size - 1) / part_size) as i32;
        self.log.info(format!(
            "multipart copy required for {:.2} GiB object: parts={parts_count} part_size={:.2} MiB",
            size as f64 / (1024.0 * 1024.0 * 1024.0),
            part_size as f64 / (1024.0 * 1024.0)
        ));
        let mut parts = Vec::with_capacity(parts_count as usize);
        let copy_source = format!("/{}/{}", bucket, escape_s3_key(key));
        for part_number in 1..=parts_count {
            let start = (part_number as i64 - 1) * part_size;
            let end = (start + part_size - 1).min(size - 1);
            let etag = self.s3_upload_part_copy(
                bucket,
                key,
                upload_id,
                part_number,
                &copy_source,
                start,
                end,
            )?;
            parts.push(MultipartPart { part_number, etag });
            if part_number == 1 || part_number == parts_count || part_number % 10 == 0 {
                self.log
                    .info(format!("multipart copy progress: part={part_number}/{parts_count}"));
            }
        }
        self.s3_complete_multipart_upload(bucket, key, upload_id, &parts)
    }

    fn s3_create_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        storage_class: &str,
    ) -> Result<String, AnyError> {
        let path = format!("/{}", escape_s3_key(key));
        let r = self.s3_signed_request(
            "POST",
            &s3_endpoint(bucket, &self.region),
            &path,
            "uploads",
            vec![
                ("x-amz-storage-class", storage_class.to_string()),
                ("content-type", "application/octet-stream".to_string()),
            ],
            vec![],
        )?;
        let body = String::from_utf8_lossy(&r.body);
        let upload_id = xml_tag_first(&body, &["UploadId"]).unwrap_or_default();
        if upload_id.is_empty() {
            return Err(format!(
                "UploadId missing in CreateMultipartUpload response: {}",
                summarize(&body)
            )
            .into());
        }
        Ok(upload_id)
    }

    fn s3_upload_part_copy(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        part_number: i32,
        copy_source: &str,
        start: i64,
        end: i64,
    ) -> Result<String, AnyError> {
        let path = format!("/{}", escape_s3_key(key));
        let raw_query = form_body(&[
            ("partNumber", &part_number.to_string()),
            ("uploadId", upload_id),
        ]);
        let r = self.s3_signed_request(
            "PUT",
            &s3_endpoint(bucket, &self.region),
            &path,
            &raw_query,
            vec![
                ("x-amz-copy-source", copy_source.to_string()),
                ("x-amz-copy-source-range", format!("bytes={start}-{end}")),
            ],
            vec![],
        )?;
        let body = String::from_utf8_lossy(&r.body);
        let etag = xml_tag_first(&body, &["ETag"]).unwrap_or_default();
        if etag.is_empty() {
            return Err(format!(
                "ETag missing in UploadPartCopy response: {}",
                summarize(&body)
            )
            .into());
        }
        Ok(etag)
    }

    fn s3_complete_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: &[MultipartPart],
    ) -> Result<(), AnyError> {
        let path = format!("/{}", escape_s3_key(key));
        let raw_query = form_body(&[("uploadId", upload_id)]);
        let mut body =
            String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
        for p in parts {
            body.push_str("<Part><PartNumber>");
            body.push_str(&p.part_number.to_string());
            body.push_str("</PartNumber><ETag>");
            body.push_str(&xml_escape(&p.etag));
            body.push_str("</ETag></Part>");
        }
        body.push_str("</CompleteMultipartUpload>");
        self.s3_signed_request(
            "POST",
            &s3_endpoint(bucket, &self.region),
            &path,
            &raw_query,
            vec![("content-type", "application/xml".to_string())],
            body.into_bytes(),
        )?;
        Ok(())
    }

    fn s3_abort_multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<(), AnyError> {
        let path = format!("/{}", escape_s3_key(key));
        let raw_query = form_body(&[("uploadId", upload_id)]);
        self.s3_signed_request(
            "DELETE",
            &s3_endpoint(bucket, &self.region),
            &path,
            &raw_query,
            vec![],
            vec![],
        )?;
        Ok(())
    }
}
