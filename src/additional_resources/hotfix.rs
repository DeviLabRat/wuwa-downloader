use std::collections::{BTreeMap, HashSet};

use reqwest::Client;
use serde_json::Value;
use sha1::{Digest, Sha1};

use super::{
    AdditionalResourcesSettings, ResourceContext, ask_quality_for, fetch_bytes, manifest,
    newest_config, optional_config,
};
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
    quality: &mut Option<String>,
    settings: &AdditionalResourcesSettings,
    context: &ResourceContext,
) -> Result<HotfixPlan, String> {
    println!(
        "{} Fetching core hotfix configuration for Windows {}...",
        Status::info(),
        context.package_version
    );
    let urls = |name: &str| {
        let route = format!("{}/Windows/{name}", context.mix);
        context
            .prefixes
            .iter()
            .map(|p| build_download_url(p, &route))
            .collect()
    };
    let base = newest_config(client, urls("config.json"), &settings.crypto, "UpdateTime").await?;
    let sd = if manifest::supports_quality(&context.package_version)? {
        optional_config(
            client,
            urls("config_sd.json"),
            &settings.crypto,
            "UpdateTime",
        )
        .await
    } else {
        Ok(None)
    };
    let mut configs = vec![("config.json".to_string(), None, base)];
    if !matches!(sd, Ok(None)) {
        let selected = match quality {
            Some(selected) => selected,
            None => quality.insert(ask_quality_for("Resource quality")?),
        };
        let name = format!("config_{}.json", selected.to_ascii_lowercase());
        let config = if selected == "SD" {
            sd?.ok_or("Missing SD configuration")?
        } else {
            newest_config(client, urls(&name), &settings.crypto, "UpdateTime")
                .await
                .map_err(|error| match &sd {
                    Err(probe_error) => {
                        format!("{error}; SD configuration probe failed: {probe_error}")
                    }
                    _ => error,
                })?
        };
        configs.push((name, Some(selected.to_string()), config));
    }
    let mut plan = HotfixPlan {
        resources: Vec::new(),
        versions: BTreeMap::new(),
    };
    for (config_name, tier, config) in configs {
        for (key, label) in [("launcher", "Launcher"), ("resource", "Resource")] {
            if tier.is_some() && key == "launcher" {
                continue;
            }
            let (version, hashes) = if let Some(versions) = config.get("ResVersions") {
                let info = versions
                    .get(key)
                    .ok_or_else(|| format!("Missing {key} in {config_name}"))?;
                (info.get("Version"), info.get("IndexSha1"))
            } else {
                (
                    config.get(format!("{label}Version")),
                    config.get(format!("{label}IndexSha1")),
                )
            };
            let version = version
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Missing {label}Version in {config_name}"))?;
            manifest::validate_version(version)?;
            if version.rsplit_once('.').map(|(prefix, _)| prefix)
                != context
                    .package_version
                    .rsplit_once('.')
                    .map(|(prefix, _)| prefix)
            {
                return Err("Core resource version belongs to a different game package".into());
            }
            let hash = hashes
                .and_then(|hashes| hashes.get(version))
                .and_then(Value::as_str);
            let hash = match hash {
                Some(hash) if !hash.is_empty() => hash,
                _ if version == context.package_version => continue,
                _ => return Err(format!("Missing manifest SHA-1 for {key} {version}")),
            };
            manifest::validate_sha1(hash)?;
            let group = match tier {
                Some(ref tier) => format!("{label}_{tier}"),
                None => label.to_string(),
            };
            let legacy = config.get("ResVersions").is_none();
            let route = if legacy {
                let prefix = if key == "launcher" {
                    "UpdaterPatchList"
                } else {
                    "PatchList"
                };
                format!("{}/Windows/{prefix}_{version}.txt", context.res)
            } else {
                format!("{}/Windows/{version}/Manifest{group}.txt", context.mix)
            };
            let mut found = None;
            let mut last_error = String::new();
            for prefix in &context.prefixes {
                let url = build_download_url(prefix, &route);
                let result = async {
                    let bytes = fetch_bytes(client, &url).await?;
                    if !format!("{:x}", Sha1::digest(&bytes)).eq_ignore_ascii_case(hash) {
                        return Err(format!("Core manifest SHA-1 mismatch from {prefix}"));
                    }
                    let data = settings.crypto.decode(&bytes)?;
                    if legacy {
                        return parse_patch_list(
                            &data,
                            &context.package_version,
                            &group,
                            &context.res,
                        );
                    }
                    parse_manifest(
                        &data,
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

fn parse_patch_list(
    data: &Value,
    package: &str,
    group: &str,
    res_route: &str,
) -> Result<Vec<ResourceItem>, String> {
    manifest::validate_version(package)?;
    if !matches!(group, "Launcher" | "Resource") || !manifest::safe_component(res_route) {
        return Err("Invalid legacy resource group or route".into());
    }
    let rows = data
        .get(package)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Patch list has no entries for installed package {package}"))?;
    let mut resources = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let row = row.as_str().ok_or("Invalid legacy patch list row")?;
        if row.is_empty() {
            continue;
        }
        let fields: Vec<_> = row.split(',').collect();
        if fields.len() < 12 {
            return Err("Incomplete legacy patch list row".into());
        }
        let stem = fields[0]
            .strip_suffix(".pak")
            .ok_or("Invalid legacy PAK filename")?;
        if !manifest::safe_component(stem) || !seen.insert(stem.to_string()) {
            return Err("Invalid or duplicate legacy patch filename".into());
        }
        let mut has_pak = false;
        let mut has_sig = false;
        for (extension, size_field, hash_field) in [
            ("pak", 4, 5),
            ("utoc", 6, 7),
            ("ucas", 8, 9),
            ("sig", 10, 11),
        ] {
            let size: u64 = fields[size_field]
                .parse()
                .map_err(|_| "Invalid legacy patch file size")?;
            if size == 0 {
                continue;
            }
            let hash = fields[hash_field];
            manifest::validate_sha1(hash)?;
            has_pak |= extension == "pak";
            has_sig |= extension == "sig";
            let name = format!("{stem}.{extension}");
            resources.push(ResourceItem {
                dest: format!("Client/Saved/Resources/{package}/{group}/{name}"),
                remote_path: Some(format!("{res_route}/Windows/{name}")),
                checksum: Some(Checksum::Sha1(hash.to_string())),
                size: Some(size),
            });
        }
        if has_pak && !has_sig {
            return Err(format!(
                "Legacy patch list is missing the signature for {stem}"
            ));
        }
    }
    Ok(resources)
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
