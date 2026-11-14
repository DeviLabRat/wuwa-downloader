use std::collections::{BTreeMap, HashSet};

use reqwest::Client;
use serde_json::Value;
use sha1::{Digest, Sha1};

use super::{AdditionalResourcesSettings, ResourceContext, fetch_bytes, manifest, newest_config};
use crate::{
    config::cfg::{Checksum, ResourceItem},
    config::status::Status,
    network::client::build_download_url,
};

pub struct HotfixPlan {
    pub resources: Vec<ResourceItem>,
    pub versions: BTreeMap<String, String>,
}

pub(super) async fn discover_core(
    client: &Client,
    quality: &str,
    settings: &AdditionalResourcesSettings,
    context: &ResourceContext,
) -> Result<HotfixPlan, String> {
    let quality = quality.to_ascii_uppercase();
    if !matches!(quality.as_str(), "SD" | "HD" | "UHD") {
        return Err("Resource quality must be SD, HD or UHD".into());
    }
    println!(
        "{} Fetching core hotfix configuration for Windows {} ({quality})...",
        Status::info(),
        context.package_version
    );
    let mut plan = HotfixPlan {
        resources: Vec::new(),
        versions: BTreeMap::new(),
    };
    for (config_name, tier) in [
        ("config.json".to_string(), None),
        (
            format!("config_{}.json", quality.to_ascii_lowercase()),
            Some(quality.as_str()),
        ),
    ] {
        let route = format!("{}/Windows/{config_name}", context.mix);
        let urls = context
            .prefixes
            .iter()
            .map(|p| build_download_url(p, &route))
            .collect();
        let config = newest_config(client, urls, &settings.crypto, "UpdateTime").await?;
        if config.get("PackageVersion").and_then(Value::as_str)
            != Some(context.package_version.as_str())
        {
            return Err(format!(
                "Core configuration {config_name} does not match installed package {}",
                context.package_version
            ));
        }
        for (key, label) in [("launcher", "Launcher"), ("resource", "Resource")] {
            if tier.is_some() && key == "launcher" {
                continue;
            }
            let info = config
                .pointer(&format!("/ResVersions/{key}"))
                .ok_or_else(|| format!("Missing {key} in {config_name}"))?;
            let version = info
                .get("Version")
                .and_then(Value::as_str)
                .ok_or("Missing core resource version")?;
            manifest::validate_version(version)?;
            if version.rsplit_once('.').map(|(prefix, _)| prefix)
                != context
                    .package_version
                    .rsplit_once('.')
                    .map(|(prefix, _)| prefix)
            {
                return Err("Core resource version belongs to a different game package".into());
            }
            let hash = info
                .get("IndexSha1")
                .and_then(|hashes| hashes.get(version))
                .and_then(Value::as_str);
            let hash = match hash {
                Some(hash) if !hash.is_empty() => hash,
                _ if version == context.package_version => continue,
                _ => return Err(format!("Missing manifest SHA-1 for {key} {version}")),
            };
            manifest::validate_sha1(hash)?;
            let group = match tier {
                Some(tier) => format!("{label}_{tier}"),
                None => label.to_string(),
            };
            let route = format!("{}/Windows/{version}/Manifest{group}.txt", context.mix);
            let mut found = None;
            let mut last_error = String::new();
            for prefix in &context.prefixes {
                let url = build_download_url(prefix, &route);
                let result = async {
                    let bytes = fetch_bytes(client, &url).await?;
                    if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(hash) {
                        return Err(format!("Core manifest SHA-1 mismatch from {prefix}"));
                    }
                    parse_manifest(
                        &settings.crypto.decode(&bytes)?,
                        &context.package_version,
                        version,
                        &group,
                        &context.res,
                    )
                }
                .await;
                match result {
                    Ok(items) => {
                        found = Some(items);
                        break;
                    }
                    Err(error) => last_error = error,
                }
            }
            let items = found
                .ok_or_else(|| format!("No valid {group} manifest for {version}: {last_error}"))?;
            plan.resources.extend(items);
            plan.versions.insert(group, version.to_string());
        }
    }
    Ok(plan)
}

pub fn parse_manifest(
    data: &Value,
    package: &str,
    version: &str,
    group: &str,
    res_route: &str,
) -> Result<Vec<ResourceItem>, String> {
    manifest::validate_version(package)?;
    manifest::validate_version(version)?;
    if !matches!(
        group,
        "Launcher" | "Resource" | "Resource_SD" | "Resource_HD" | "Resource_UHD"
    ) || !manifest::safe_component(res_route)
    {
        return Err("Invalid core resource group or route".into());
    }
    if data.get("Version").and_then(Value::as_str) != Some(version) {
        return Err("Core manifest version does not match the selected update".into());
    }
    if data.get("Pv").is_some_and(|pv| pv.as_u64() != Some(1)) {
        return Err("Unsupported core manifest format version".into());
    }
    let patch = data
        .get("DiffPatch")
        .ok_or("Missing DiffPatch in core manifest")?;
    let mut resources = Vec::new();
    for (field, local_version, remote_version) in [
        ("BaseFiles", "Base", package),
        ("PatchFiles", version, version),
    ] {
        let files = patch
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("Missing {field} in core manifest"))?;
        let mut seen = HashSet::new();
        for file in files {
            let name = file
                .get("Name")
                .and_then(Value::as_str)
                .ok_or("Missing core filename")?;
            if !manifest::safe_component(name) || !seen.insert(name) {
                return Err("Invalid or duplicate core filename".into());
            }
            if ![".pak", ".sig", ".utoc", ".ucas"]
                .iter()
                .any(|extension| name.ends_with(extension))
            {
                return Err(format!("Unsupported core target file: {name}"));
            }
            let hash = file
                .get("Hash")
                .and_then(Value::as_str)
                .ok_or("Missing core file SHA-1")?;
            manifest::validate_sha1(hash)?;
            resources.push(ResourceItem {
                dest: format!("Client/Saved/Resources/{package}/{group}/{local_version}/{name}"),
                remote_path: Some(format!("{res_route}/Windows/{remote_version}/{name}")),
                checksum: Some(Checksum::Sha1(hash.to_string())),
                size: Some(manifest::size(file.get("Size"))?),
            });
        }
        for name in &seen {
            if let Some(stem) = name.strip_suffix(".pak")
                && !seen.contains(format!("{stem}.sig").as_str())
            {
                return Err(format!("Core manifest is missing the signature for {name}"));
            }
        }
    }
    Ok(resources)
}
