mod additional;
mod bootstrap;
mod hotfix;
pub use additional::discover_resources;
mod crypto;
mod keys;
mod manifest;
mod settings;
pub use settings::AdditionalResourcesSettings;

use std::{io::Write, path::Path, time::Duration};

use colored::Colorize;
use reqwest::Client;
use serde_json::Value;
use sha1::{Digest, Sha1};
use tokio::task::JoinSet;

use crate::{
    config::cfg::{Config, ResourceItem},
    config::status::Status,
    io::util::read_line,
    network::client::build_download_url,
};
use crypto::RemoteCrypto;

pub fn validate_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|e| format!("Invalid resource URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Resource URLs must use HTTP or HTTPS".into());
    }
    Ok(())
}

async fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>, String> {
    validate_url(url)?;
    let mut response = client
        .get(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Request failed for {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP failure for {url}: {e}"))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
            return Err(format!("Configuration too large: {url}"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn newest_config(
    client: &Client,
    urls: Vec<String>,
    crypto: &RemoteCrypto,
    timestamp: &str,
) -> Result<Value, String> {
    let mut tasks = JoinSet::new();
    for url in urls {
        let client = client.clone();
        let crypto = crypto.clone();
        tasks.spawn(async move { crypto.decode(&fetch_bytes(&client, &url).await?) });
    }
    let mut best = None;
    let mut latest = 0;
    let mut last_error = "No configuration URLs".to_string();
    while let Some(result) = tasks.join_next().await {
        match result.map_err(|e| e.to_string()).and_then(|v| v) {
            Ok(data) => match data.get(timestamp).and_then(Value::as_u64) {
                Some(time) if best.is_none() || time > latest => {
                    latest = time;
                    best = Some(data);
                }
                Some(_) => {}
                None => last_error = format!("Configuration is missing {timestamp}"),
            },
            Err(error) => last_error = error,
        }
    }
    best.ok_or_else(|| format!("All configuration mirrors failed: {last_error}"))
}

fn entry_fields<'a>(
    entry: &'a Value,
    parallel: Option<&str>,
) -> Result<(Vec<String>, &'a str, &'a str), String> {
    let default = entry
        .get("default")
        .ok_or("Remote entry has no default configuration")?;
    let override_config = parallel.and_then(|name| entry.get(name));
    if parallel.is_some() && override_config.is_none() {
        return Err("Configured parallel entry does not exist in the bootstrap response".into());
    }
    let field = |key: &str| {
        override_config
            .and_then(|v| v.get(key))
            .or_else(|| default.get(key))
    };
    let cdns = field("CdnUrl")
        .and_then(Value::as_array)
        .ok_or("Missing resource CDN list")?;
    let prefixes: Vec<_> = cdns
        .iter()
        .filter_map(|v| v.get("url").and_then(Value::as_str))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    if prefixes.is_empty() {
        return Err("No resource CDN URLs".into());
    }
    for prefix in &prefixes {
        validate_url(prefix)?;
    }
    let mix = field("MixUri")
        .and_then(Value::as_str)
        .ok_or("Missing MixUri")?;
    let res = field("ResUri")
        .and_then(Value::as_str)
        .ok_or("Missing ResUri")?;
    if !manifest::safe_component(mix) || !manifest::safe_component(res) {
        return Err("Invalid resource route in remote entry".into());
    }
    Ok((prefixes, mix, res))
}

struct ResourceContext {
    prefixes: Vec<String>,
    mix: String,
    res: String,
    package_version: String,
}

impl ResourceContext {
    fn config(&self) -> Config {
        Config {
            index_urls: Vec::new(),
            zip_bases: self.prefixes.clone(),
        }
    }
}

async fn resource_context(
    client: &Client,
    folder: &Path,
    settings: &AdditionalResourcesSettings,
) -> Result<ResourceContext, String> {
    let bootstrap = bootstrap::discover(folder, settings).await?;
    let entry = newest_config(client, bootstrap.entry_urls, &settings.crypto, "ModifyTime").await?;
    let (prefixes, mix, res) = entry_fields(&entry, settings.parallel.as_deref())?;
    Ok(ResourceContext {
        prefixes,
        mix: mix.to_string(),
        res: res.to_string(),
        package_version: bootstrap.package_version,
    })
}

async fn discover_videos(
    client: &Client,
    quality: &str,
    settings: &AdditionalResourcesSettings,
    context: &ResourceContext,
) -> Result<Vec<ResourceItem>, String> {
    let quality = quality.to_ascii_lowercase();
    if !matches!(quality.as_str(), "sd" | "hd" | "uhd") {
        return Err("Video quality must be SD, HD or UHD".into());
    }
    println!(
        "{} Fetching video configuration for Windows {} ({})...",
        Status::info(),
        context.package_version,
        quality.to_uppercase()
    );
    let prefixes = &context.prefixes;
    let mix = &context.mix;
    let res = &context.res;
    let config_route = format!("{mix}/Windows/VideoConfig_{quality}.json");
    let config_urls = prefixes
        .iter()
        .map(|p| build_download_url(p, &config_route))
        .collect();
    let config = newest_config(client, config_urls, &settings.crypto, "UpdateTime").await?;
    let expected = config
        .get("IndexSha1")
        .and_then(Value::as_str)
        .ok_or("Video configuration has no IndexSha1")?;
    manifest::validate_sha1(expected)?;
    let route = format!(
        "{mix}/Windows/{}/Video/VideoManifest_{quality}.json",
        context.package_version
    );
    let mut last_error = String::new();
    for prefix in prefixes {
        let url = build_download_url(prefix, &route);
        let result = async {
            let bytes = fetch_bytes(client, &url).await?;
            if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(expected) {
                return Err(format!("Video manifest SHA-1 mismatch from {prefix}"));
            }
            manifest::parse_manifest(&settings.crypto.decode(&bytes)?, &quality)
        }
        .await;
        match result {
            Ok(mut resources) => {
                for item in &mut resources {
                    let filename = item
                        .remote_path
                        .as_deref()
                        .ok_or("Missing video remote path")?;
                    item.remote_path = Some(format!(
                        "{res}/Windows/{}/Video/{filename}",
                        context.package_version
                    ));
                }
                return Ok(resources);
            }
            Err(error) => last_error = error,
        }
    }
    Err(format!(
        "No valid video manifest for package {}: {last_error}",
        context.package_version
    ))
}

fn choose_resources(resources: &[ResourceItem]) -> Result<Option<Vec<ResourceItem>>, String> {
    let bytes: u64 = resources.iter().filter_map(|i| i.size).sum();
    println!(
        "\n{} Found {} video packages ({:.2} GiB)\n",
        Status::info(),
        (resources.len() / 2).to_string().cyan(),
        bytes as f64 / 1024_f64.powi(3)
    );
    println!("{} 1. All videos", Status::question());
    println!("{} 2. Specific packages", Status::question());
    println!("{} 3. Cancel", Status::question());
    loop {
        print!(
            "\n{} Choose videos to download (1, 2 or 3): ",
            Status::question()
        );
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let input = read_line().map_err(|e| e.to_string())?;
        if input.is_empty() {
            return Err("Input closed before video selection".into());
        }
        match input.trim() {
            "1" => return Ok(Some(resources.to_vec())),
            "2" => {
                let ids: std::collections::BTreeSet<_> = resources
                    .iter()
                    .filter_map(|item| item.dest.rsplit('/').nth(1))
                    .collect();
                println!(
                    "{} Available package IDs: {}",
                    Status::info(),
                    ids.into_iter().collect::<Vec<_>>().join(", ").cyan()
                );
                println!(
                    "{} Enter full IDs (e.g. 100_0), or a number to select all its variants",
                    Status::info()
                );
                loop {
                    print!(
                        "{} Enter package IDs (comma-separated): ",
                        Status::question()
                    );
                    std::io::stdout().flush().map_err(|e| e.to_string())?;
                    let ids = read_line().map_err(|e| e.to_string())?;
                    if ids.is_empty() {
                        return Err("Input closed before package selection".into());
                    }
                    match manifest::select_packages(resources, ids.trim()) {
                        Ok(items) => {
                            let size: u64 = items.iter().filter_map(|i| i.size).sum();
                            println!(
                                "{} Selected {} video packages ({:.2} GiB)",
                                Status::info(),
                                (items.len() / 2).to_string().cyan(),
                                size as f64 / 1024_f64.powi(3)
                            );
                            return Ok(Some(items));
                        }
                        Err(e) => println!("{} {e}", Status::error()),
                    }
                }
            }
            "3" => return Ok(None),
            _ => println!("{} Invalid choice, please enter 1, 2 or 3", Status::error()),
        }
    }
}

pub fn ask_quality() -> Result<String, String> {
    ask_quality_for("Video quality")
}

fn ask_quality_for(label: &str) -> Result<String, String> {
    println!("\n{} {label}:", Status::info());
    println!("1. SD (Standard)\n2. HD (High)\n3. UHD (Ultra High)");
    loop {
        print!("{} Select quality (1, 2 or 3): ", Status::question());
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let input = read_line().map_err(|e| e.to_string())?;
        if input.is_empty() {
            return Err("Input closed before quality selection".into());
        }
        match input.trim() {
            "1" => return Ok("SD".into()),
            "2" => return Ok("HD".into()),
            "3" => return Ok("UHD".into()),
            _ => println!("{} Invalid choice, please enter 1, 2 or 3", Status::error()),
        }
    }
}
