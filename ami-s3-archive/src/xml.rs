pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn xml_tag_first(body: &str, tags: &[&str]) -> Option<String> {
    tags.iter().find_map(|tag| xml_tag_one(body, tag))
}

pub fn xml_tag_one(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(xml_unescape(body[start..end].trim()))
}

pub fn xml_tags_all(body: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut offset = 0;
    while let Some(i) = body[offset..].find(&open) {
        let start = offset + i + open.len();
        if let Some(j) = body[start..].find(&close) {
            let end = start + j;
            out.push(xml_unescape(body[start..end].trim()));
            offset = end + close.len();
        } else {
            break;
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn xml_item_chunks(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(i) = body[offset..].find("<item>") {
        let start = offset + i;
        if let Some(j) = body[start..].find("</item>") {
            let end = start + j + "</item>".len();
            out.push(&body[start..end]);
            offset = end;
        } else {
            break;
        }
    }
    out
}

pub fn parse_store_image_tasks(body: &str) -> Vec<crate::types::StoreImageTask> {
    xml_item_chunks(body)
        .into_iter()
        .filter_map(|item| {
            let image_id = xml_tag_first(item, &["amiId", "imageId"])?;
            if image_id.is_empty() {
                return None;
            }
            Some(crate::types::StoreImageTask {
                image_id,
                bucket: xml_tag_first(item, &["bucket"]).unwrap_or_default(),
                object_key: xml_tag_first(item, &["s3objectKey", "s3ObjectKey", "objectKey"])
                    .unwrap_or_default(),
                state: xml_tag_first(item, &["storeTaskState", "state"]).unwrap_or_default(),
                progress: xml_tag_first(item, &["storeTaskProgressPercentage", "progressPercentage"])
                    .unwrap_or_default(),
            })
        })
        .collect()
}

pub fn parse_delete_snapshot_results(body: &str) -> Vec<(String, String)> {
    xml_item_chunks(body)
        .into_iter()
        .filter_map(|item| {
            let snap = xml_tag_first(item, &["snapshotId"])?;
            if snap.is_empty() {
                return None;
            }
            let code = xml_tag_first(item, &["returnCode"]).unwrap_or_default();
            Some((snap, code))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_store_image_task() {
        let body = r#"<item><imageId>ami-abc</imageId><bucket>b</bucket><objectKey>k.bin</objectKey><storeTaskState>Completed</storeTaskState></item>"#;
        let tasks = parse_store_image_tasks(body);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].image_id, "ami-abc");
        assert_eq!(tasks[0].state, "Completed");
    }
}
