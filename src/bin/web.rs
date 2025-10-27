use std::{net::SocketAddr, path::PathBuf, sync::LazyLock};

use askama::Template;
use axum::{
    body::Body, extract::{OriginalUri, Path, Query, State}, http::{header, HeaderMap, HeaderName, HeaderValue, Response, Uri}, response::{Html, IntoResponse, Redirect}, routing::get, Router
};
use clap::Parser;
use fjall::{Config, PartitionCreateOptions, PartitionHandle};
use include_dir::{Dir, include_dir};
use jiff::{Timestamp, tz::TimeZone};
use reqwest::StatusCode;
use rfa::{get_filename_from_url, kv_sep_partition_option, site_code};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// RFA backup website
#[derive(Parser, Debug)]
struct Args {
    /// listening address
    #[arg(short, long, default_value = "127.0.0.1:3333")]
    addr: String,

    /// data folder, containing imgs/ and rfa.db/
    #[arg(short = 'd', long, default_value = "rfa_data")]
    data: String,
}

static ARGS: LazyLock<Args> = LazyLock::new(|| Args::parse());

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let folder = PathBuf::from(&ARGS.data);
    let db_folder = folder.join("rfa.db");

    let keyspace = Config::new(db_folder).open().unwrap();
    let db = keyspace
        .open_partition("rfa", kv_sep_partition_option())
        .unwrap();
    let index = keyspace
        .open_partition("index", PartitionCreateOptions::default())
        .unwrap();
    let app_state = AppState { db, index };

    let addr: SocketAddr = ARGS.addr.parse().unwrap();
    info!("Listening to {addr}");


    let img_folder = folder.join("imgs");
    let app = Router::new()
        .route("/", get(home))
        .route("/{site}", get(site))
        .route("/{site}/{*id}", get(page))
        .route("/style.css", get(style))
        .route("/static/logo/{filename}", get(serve_logo))
        .nest_service("/imgs", ServeDir::new(img_folder))
        .with_state(app_state)
        .fallback(handler_404);

    let listener = TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn page(
    State(state): State<AppState>,
    OriginalUri(original_uri): OriginalUri,
    Query(params): Query<SiteParams>,
) -> impl IntoResponse {
    let original_uri = original_uri.to_string();
    let key = original_uri.split("?").next().unwrap().trim_matches('/');
    info!("page: {key}");
    if let Some(v) = state.db.get(key).unwrap() {
        let content = String::from_utf8_lossy(&v);
        let json: Value = serde_json::from_str(&content).unwrap();
        let article: Article = (&json).into();
        into_response(&article)
    } else {
        if let Some((site, _)) = key.split_once('/') {
            let page = params.page.unwrap_or_default();
            let mut items = vec![];
            let n = page * 20;
            for (idx, i) in state.db.prefix(key).rev().enumerate() {
                if idx < n {
                    continue;
                }
                if idx >= n + 20 {
                    break;
                }
                let (_, v) = i.unwrap();
                let json: Value = serde_json::from_slice(&v).unwrap();
                let item: Item = (&json).into();
                items.push(item);
            }
            let url_path = format!("/{key}");
            let page_list = PageList {
                items,
                site: site.to_owned(),
                page: page + 1,
                url_path,
            };
            into_response(&page_list)
        } else {
            error!("{} not found", key);
            (StatusCode::NOT_FOUND, "Not found").into_response()
        }
    }
}

async fn home() -> impl IntoResponse {
    Redirect::to("/english")
}

#[derive(Deserialize)]
struct SiteParams {
    page: Option<usize>,
}

#[derive(Debug, Serialize)]
enum ContentType {
    Text(String),
    Image(String, String),
    Header(String),
    #[allow(dead_code)]
    Other,
}

#[derive(Template, Debug, Serialize)]
#[template(path = "article.html", escape = "none")]
struct Article {
    site: String,
    item: Item,
    author: Option<String>,
    contents: Vec<ContentType>,
}

impl From<&Value> for Article {
    fn from(json: &Value) -> Self {
        let item: Item = json.into();
        let site = item
            .website_url
            .trim_start_matches('/')
            .split_once('/')
            .unwrap()
            .0
            .to_owned();
        let author = json
            .get("credits")
            .and_then(|p| p.get("by"))
            .and_then(|b| b.as_array())
            .and_then(|a| a.get(0))
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_owned());

        let mut contents = vec![];
        if let Some(content_elements) = json["content_elements"].as_array() {
            for c in content_elements {
                match c["type"].as_str().unwrap() {
                    "text" => {
                        let content = c["content"].as_str().unwrap();
                        if !content.is_empty() {
                            contents.push(ContentType::Text(content.to_owned()))
                        }
                    }
                    "image" => {
                        let url = c["url"].as_str().unwrap();
                        let img_name = get_filename_from_url(url);
                        let url = format!("/imgs/{img_name}");
                        let caption = c["caption"].as_str().unwrap_or_default();
                        contents.push(ContentType::Image(url, caption.to_owned()))
                    }
                    "header" => {
                        let content = c["content"].as_str().unwrap();
                        if !content.is_empty() {
                            contents.push(ContentType::Header(content.to_owned()))
                        }
                    }
                    _ => {
                        warn!("{} -> unknown content type: {c}", item.website_url)
                    }
                }
            }
        }

        Self {
            site,
            item,
            author,
            contents,
        }
    }
}

async fn site(
    Path(site): Path<String>,
    Query(params): Query<SiteParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let index = state.index;
    let db = state.db;
    let mut items = Vec::with_capacity(20);
    let code = site_code(&site);
    let page = params.page.unwrap_or(0);
    info!("site:{site} -> page:{page}");
    let n = page * 20;
    for (idx, i) in index.prefix([code]).rev().enumerate() {
        if idx < n {
            continue;
        }
        if idx >= n + 20 {
            break;
        }
        let (k, _) = i.unwrap();
        let rest = String::from_utf8_lossy(&k[9..]);
        let path = format!("{site}/{rest}");
        if let Some(v) = db.get(&path).unwrap() {
            let json: Value = serde_json::from_slice(&v).unwrap();
            let item: Item = (&json).into();
            items.push(item)
        }
    }

    let url_path = format!("/{}", site);
    let page_list = PageList {
        items,
        site,
        page: page + 1,
        url_path,
    };
    into_response(&page_list)
}

async fn handler_404(uri: Uri) -> impl IntoResponse {
    error!("No route for {}", uri);
    (
        StatusCode::NOT_FOUND,
        Html("404 NOT FOUND.<br>Back to <a href='/'>Home</a>"),
    )
}

#[derive(Clone)]
struct AppState {
    db: PartitionHandle,
    index: PartitionHandle,
}

#[derive(Debug, Serialize)]
struct Item {
    headlines: String,
    display_date: String,
    description: String,
    promo_img: Option<String>,
    caption: Option<String>,
    website_url: String,
    section: (String, String),
}

impl From<&Value> for Item {
    fn from(json: &Value) -> Self {
        let headlines = json["headlines"]["basic"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let display_date = json["display_date"].as_str().unwrap_or_default();
        let ts: Timestamp = display_date.parse().unwrap();
        let display_date = ts.to_zoned(TimeZone::UTC).strftime("%Y-%m-%d").to_string();

        let description = json["description"]["basic"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let promo_img = json
            .get("promo_items")
            .and_then(|p| p.get("basic"))
            .and_then(|b| b.get("url"))
            .and_then(|img| img.as_str())
            .map(|s| {
                let img_name = get_filename_from_url(s);
                format!("/imgs/{img_name}")
            });

        let caption = json
            .get("promo_items")
            .and_then(|p| p.get("basic"))
            .and_then(|b| b.get("caption"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_owned());

        let mut id = String::new();
        let mut name = String::new();
        let mut website_url = String::new();
        if let Some(obj) = json["websites"].as_object() {
            if let Some((_, value)) = obj.iter().next() {
                website_url = value["website_url"].as_str().unwrap().to_owned();
                let section = value.get("website_section").unwrap();
                id = section
                    .get("_id")
                    .unwrap_or_default()
                    .as_str()
                    .unwrap_or_default()
                    .replace("world/asia/", "");
                name = section
                    .get("name")
                    .unwrap_or_default()
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
            }
        }

        let item = Item {
            headlines,
            display_date,
            description,
            promo_img,
            caption,
            website_url,
            section: (id, name),
        };

        item
    }
}

#[derive(Template)]
#[template(path = "list.html")]
struct PageList {
    site: String,
    items: Vec<Item>,
    page: usize,
    url_path: String,
}

fn into_response<T: Template>(t: &T) -> Response<Body> {
    match t.render() {
        Ok(body) => Html(body).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn style() -> impl IntoResponse {
    let headers = [
        (header::CONTENT_TYPE, "text/css"),
        (
            header::CACHE_CONTROL,
            "public, max-age=1209600, s-maxage=86400",
        ),
    ];

    (headers, include_str!("../../static/style.css"))
}

static STATIC_LOGO_DIR: Dir = include_dir!("static/logo");

async fn serve_logo(Path(filename): Path<String>) -> impl IntoResponse {
    if let Some(file) = STATIC_LOGO_DIR.get_file(&filename) {
        let body = file.contents();

        let mut headers = HeaderMap::new();
        headers.insert("Content-Type", "image/png".parse().unwrap());
        headers.insert(
            HeaderName::from_static("cache-control"),
            HeaderValue::from_static("public, max-age=1209600, s-maxage=86400"),
        );

        (headers, body).into_response()
    } else {
        (StatusCode::NOT_FOUND, "File not found").into_response()
    }
}
