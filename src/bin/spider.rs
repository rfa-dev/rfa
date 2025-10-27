use clap::Parser;
use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};
use jiff::{
    ToSpan,
    civil::{Date, date},
};
use reqwest::Proxy;
use rfa::{get_filename_from_url, index_key, kv_sep_partition_option};
use serde_json::Value;
use std::{
    error::Error,
    fs::create_dir_all,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tracing::{error, info, instrument};
use urlencoding::encode;

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let mut client_builder = reqwest::Client::builder();
    if let Some(proxy) = &ARGS.proxy {
        client_builder = client_builder.proxy(Proxy::all(proxy).unwrap());
    }
    let retry = reqwest::retry::for_host("www.rfa.org").max_retries_per_request(10);
    client_builder
        .retry(retry)
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap()
});

const SIZE: u64 = 100;

const SITE_LIST: [&str; 10] = [
    "radio-free-asia", // English
    "rfa-mandarin",
    "rfa-cantonese",
    "rfa-burmese",
    "rfa-korean",
    "rfa-lao",
    "rfa-khmer",
    "rfa-tibetan",
    "rfa-uyghur",
    "rfa-vietnamese",
];

/// RFA website crawler, downloading lists, pages and imgs
#[derive(Parser, Debug)]
struct Args {
    /// Website to fetch (e.g., rfa-mandarin, rfa-korean)
    #[arg(short = 'w', long, value_delimiter = ',', help = SITE_LIST.join(","))]
    sites: Vec<String>,

    /// proxy (e.g., http://127.0.0.1:8089)
    #[clap(long)]
    proxy: Option<String>,

    #[arg(short = 'o', long, default_value = "rfa_data")]
    output: String,
}

static ARGS: LazyLock<Args> = LazyLock::new(|| Args::parse());
static SITES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let sites = if ARGS.sites.is_empty() {
        info!("No website specified, fetching all available websites.");
        SITE_LIST.iter().map(|s| s.to_string()).collect()
    } else {
        for site in &ARGS.sites {
            let site = site.trim().to_lowercase();
            if !SITE_LIST.contains(&site.as_str()) {
                panic!(
                    "Unknown website: {}, available options are: {:?}",
                    site, SITE_LIST
                );
            }
        }
        ARGS.sites.clone()
    };
    sites
});

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let path = Path::new(&ARGS.output);
    if !path.exists() {
        create_dir_all(path)?;
    }
    std::env::set_current_dir(path)?;

    let keyspace = Config::new("rfa.db").open().unwrap();
    let db = keyspace
        .open_partition("rfa", kv_sep_partition_option())
        .unwrap();

    let done = keyspace
        .open_partition("done", PartitionCreateOptions::default())
        .unwrap();

    let index = keyspace
        .open_partition("index", PartitionCreateOptions::default())
        .unwrap();

    for site in &*SITES {
        info!("Processing website: {}", site);
        // begin from 1998-01 to 2025-09
        let mut start_date = date(1998, 1, 1);
        let end_date = date(2025, 9, 30);
        while start_date <= end_date {
            fetch_articles(
                &keyspace,
                &db,
                &done,
                &index,
                site,
                start_date.year(),
                start_date.month(),
            )
            .await?;
            start_date = start_date.saturating_add(1.month());
        }
    }

    Ok(())
}

#[instrument(skip(keyspace, db, done, index))]
async fn fetch_articles(
    keyspace: &Keyspace,
    db: &PartitionHandle,
    done: &PartitionHandle,
    index: &PartitionHandle,
    site: &str,
    year: i16,
    month: i8,
) -> Result<(), Box<dyn Error>> {
    let done_key = format!("{site}-{year}-{month}");
    if done.contains_key(&done_key).unwrap() {
        info!("Already download.");
        return Ok(());
    }

    let begin = date(year, month, 1);
    let end = begin.last_of_month();
    let offset = 0;
    let json = req_story_archive(site, offset, &begin, &end).await?;

    let count = json["count"].as_u64().unwrap();
    info!("Total articles found: {}", count);

    if count == 0 {
        if year < 2024 {
            done.insert(&done_key, &[]).unwrap();
        }
        return Ok(());
    }

    let (mut items, mut imgs) = extract(&json);

    while count > items.len() as u64 {
        let offset = items.len() as u64;
        let json = req_story_archive(site, offset, &begin, &end).await?;
        let (items2, imgs2) = extract(&json);

        items.extend(items2);
        imgs.extend(imgs2);
    }

    info!("Total articles fetched: {}", items.len());

    for img in imgs {
        let img_name = get_filename_from_url(&img);
        let img_path = PathBuf::from("imgs");
        let img_path = img_path.join(img_name);

        if !Path::new(&img_path).exists() {
            if let Err(e) = dl_obj(&img, &img_path).await {
                error!("Failed to download image {}: {}", img, e);
            } else {
                info!("Downloaded image: {}", img);
            }
        } else {
            info!("Image already exists: {}", img_path.display());
        }
    }

    let mut batch = keyspace.batch();
    for i in items {
        let json: Value = serde_json::from_str(&i).unwrap();
        let website_url = json["websites"][site]["website_url"]
            .as_str()
            .unwrap()
            .trim_matches('/');

        batch.insert(&db, website_url, i);

        let display_date = json["display_date"].as_str().unwrap();
        let index_key = index_key(website_url, display_date);

        batch.insert(&index, index_key, &[]);
    }
    batch.commit().unwrap();

    done.insert(&done_key, &[]).unwrap();

    Ok(())
}

#[instrument]
async fn req_story_archive(
    site: &str,
    offset: u64,
    begin: &Date,
    end: &Date,
) -> Result<Value, Box<dyn Error>> {
    let query_json = format!(
        r#"{{"feature":"results-list","offset":{},"query":"display_date:[{} TO {}]","size":{}}}"#,
        offset, begin, end, SIZE
    );
    let encoded_query = encode(&query_json);
    let filter = format!(
        r#"{{content_elements{{_id,credits{{by{{additional_properties{{original{{byline}}}},name,type,url}}}},description{{basic}},display_date,headlines{{basic}},label{{basic{{display,text,url}}}},owner{{sponsored}},promo_items{{basic{{_id,auth{{1}},type,url,caption}},lead_art{{promo_items{{basic{{_id,auth{{1}},type,url}}}}}},type}},type,websites{{{}{{website_section{{_id,name}},website_url}}}},content_elements{{type,content,url,caption{{basic}}}}}},count,next}}"#,
        site
    );
    let filter = encode(&filter);

    let url = format!(
        "https://www.rfa.org/pf/api/v3/content/fetch/story-feed-query?query={}&filter={}&d=147&mxId=00000000&_website={}",
        encoded_query, filter, site
    );
    let resp = CLIENT.get(url).send().await?;
    info!("Status: {}", resp.status());
    let text = resp.text().await?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    Ok(json)
}

#[instrument]
async fn dl_obj(url: &str, path: &Path) -> Result<(), reqwest::Error> {
    let resp = if !url.starts_with("http") {
        error!("{url} is not valid.");
        let new_url = format!("https://www.rfa.org/{url}");
        CLIENT.get(new_url).send().await?
    } else {
        CLIENT.get(url).send().await?
    };
    info!("Status: {}", resp.status());

    let bytes = resp.bytes().await?;
    std::fs::write(path, &bytes).unwrap();
    Ok(())
}

fn extract(json: &Value) -> (Vec<String>, Vec<String>) {
    let mut items = vec![];
    let mut imgs = vec![];
    if let Some(elements) = json["content_elements"].as_array() {
        for item in elements {
            let i = serde_json::to_string(&item).unwrap();
            items.push(i);

            if let Some(promo_imgs) = item["promo_items"]["basic"]["url"].as_str() {
                imgs.push(promo_imgs.to_owned())
            }

            if let Some(contents) = item["content_elements"].as_array() {
                for content in contents {
                    if let Some(ctype) = content["type"].as_str() {
                        if ctype == "image" {
                            if let Some(img_url) = content["content"].as_str() {
                                imgs.push(img_url.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }

    (items, imgs)
}
