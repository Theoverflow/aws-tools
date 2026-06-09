#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct StoreImageTask {
    pub image_id: String,
    pub bucket: String,
    pub object_key: String,
    pub state: String,
    pub progress: String,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectHead {
    pub exists: bool,
    pub storage_class: String,
    pub content_length: i64,
}

#[derive(Debug, Clone)]
pub struct MultipartPart {
    pub part_number: i32,
    pub etag: String,
}
