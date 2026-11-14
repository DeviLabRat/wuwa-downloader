use std::collections::{BTreeSet, HashSet};

use serde_json::Value;

use crate::config::cfg::{Checksum, ResourceItem};

pub fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

pub fn validate_version(version: &str) -> Result<(), String> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("Package version must have three numeric parts, e.g. 3.8.0".into());
    }
    Ok(())
}

pub fn validate_sha1(value: &str) -> Result<(), String> {
    if value.len() != 40 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid SHA-1 in resource configuration or manifest".into());
    }
    Ok(())
}

pub(super) fn size(value: Option<&Value>) -> Result<u64, String> {
    value
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str()?.strip_prefix("__kr_long__")?.parse().ok())
        })
        .filter(|n| *n > 0)
        .ok_or_else(|| "Missing or invalid file size".into())
}

pub fn parse_manifest(data: &Value, quality: &str) -> Result<Vec<ResourceItem>, String> {
    if !matches!(quality, "" | "sd" | "hd" | "uhd") {
        return Err("Video quality must be sd, hd or uhd".into());
    }
    let packs = data
        .pointer("/VideoInfos/PakMap")
        .and_then(Value::as_object)
        .ok_or("Missing VideoInfos.PakMap in video manifest")?;
    let directory = if quality.is_empty() {
        "Client/Saved/Resources/Video/Paks".to_string()
    } else {
        format!("Client/Saved/Resources/Video/{quality}/Paks")
    };
    let mut items = Vec::new();
    let mut filenames = HashSet::new();
    for (id, pack) in packs {
        if id == "__kr_map__" && pack.is_null() {
            continue;
        }
        if !valid_package_id(id) {
            return Err(format!("Invalid video package ID: {id}"));
        }
        for (name_field, size_field, hash_field, extension) in [
            ("PakName", "PakSize", "PakHash", ".pak"),
            ("SigName", "SigSize", "SigHash", ".sig"),
        ] {
            let name = pack
                .get(name_field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Missing {name_field} for video {id}"))?;
            if !safe_component(name)
                || !name.ends_with(extension)
                || !filenames.insert(name.to_string())
            {
                return Err(format!("Invalid or duplicate video filename for {id}"));
            }
            let hash = pack
                .get(hash_field)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Missing {hash_field} for video {id}"))?;
            validate_sha1(hash)?;
            items.push(ResourceItem {
                dest: format!("{directory}/{id}/{name}"),
                remote_path: Some(name.to_string()),
                checksum: Some(Checksum::Sha1(hash.to_string())),
                size: Some(size(pack.get(size_field))?),
            });
        }
    }
    if items.is_empty() {
        return Err("Video manifest contains no packages".into());
    }
    Ok(items)
}

pub fn valid_package_id(id: &str) -> bool {
    id.split_once('_').is_some_and(|(number, gender)| {
        !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
            && matches!(gender, "0" | "1" | "2")
    })
}

pub fn select_packages(items: &[ResourceItem], input: &str) -> Result<Vec<ResourceItem>, String> {
    let requested: BTreeSet<_> = input
        .split([',', ' ', '\t'])
        .filter(|s| !s.is_empty())
        .collect();
    if requested.is_empty() {
        return Err("Enter at least one package ID".into());
    }
    let available: BTreeSet<_> = items
        .iter()
        .filter_map(|i| i.dest.rsplit('/').nth(1))
        .collect();
    let mut selected = HashSet::new();
    for id in requested {
        let matches: Vec<_> = available
            .iter()
            .filter(|candidate| {
                **candidate == id || candidate.split_once('_').is_some_and(|(n, _)| n == id)
            })
            .copied()
            .collect();
        if matches.is_empty() {
            return Err(format!("Video package {id} was not found in this manifest"));
        }
        selected.extend(matches);
    }
    Ok(items
        .iter()
        .filter(|i| {
            i.dest
                .rsplit('/')
                .nth(1)
                .is_some_and(|id| selected.contains(id))
        })
        .cloned()
        .collect())
}
