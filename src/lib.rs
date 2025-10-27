use fjall::{KvSeparationOptions, PartitionCreateOptions};
use jiff::Timestamp;

pub fn kv_sep_partition_option() -> PartitionCreateOptions {
    PartitionCreateOptions::default()
        .max_memtable_size(128_000_000)
        .with_kv_separation(
            KvSeparationOptions::default()
                .separation_threshold(750)
                .file_target_size(256_000_000),
        )
}

pub fn site_code(website: &str) -> u8 {
    match website.to_lowercase().as_ref() {
        "english" => 0,
        "mandarin" => 1,
        "cantonese" => 2,
        "burmese" => 3,
        "korean" => 4,
        "lao" => 5,
        "khmer" => 6,
        "tibetan" => 7,
        "uyghur" => 8,
        "vietnamese" => 9,
        _ => 99,
    }
}

/// site_code + ts + url_rest
pub fn index_key(website_url: &str, display_date: &str) -> Vec<u8> {
    let (website, rest) = website_url.trim_matches('/').split_once('/').unwrap();
    let code = site_code(website);

    let ts: Timestamp = display_date.parse().unwrap();
    let ts_byte = ts.as_second().to_be_bytes();

    let rest_bytes = rest.as_bytes();

    let mut key = Vec::with_capacity(1 + 8 + rest_bytes.len());
    key.push(code);
    key.extend_from_slice(&ts_byte);
    key.extend_from_slice(rest.as_bytes());

    key
}

pub fn get_filename_from_url(url: &str) -> &str {
    url.split('/')
        .next_back()
        .and_then(|s| s.split('?').next())
        .unwrap()
}
