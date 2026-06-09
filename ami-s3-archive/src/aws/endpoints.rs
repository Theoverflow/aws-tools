pub fn ec2_endpoint(region: &str) -> String {
    format!("https://ec2.{region}.amazonaws.com")
}

pub fn sts_endpoint(region: &str) -> String {
    format!("https://sts.{region}.amazonaws.com")
}

pub fn s3_endpoint(bucket: &str, region: &str) -> String {
    format!("https://{bucket}.s3.{region}.amazonaws.com")
}

pub fn escape_s3_key(key: &str) -> String {
    url::form_urlencoded::byte_serialize(key.as_bytes())
        .collect::<String>()
        .replace("%2F", "/")
}

pub fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}
