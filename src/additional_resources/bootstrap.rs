use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

use serde_json::Value;
use tokio::process::Command;

use super::{
    AdditionalResourcesSettings,
    manifest::{safe_component, validate_version},
};
use crate::network::client::build_download_url;

pub struct Bootstrap {
    pub entry_urls: Vec<String>,
    pub package_version: String,
}

fn ini(text: &str) -> HashMap<&str, &str> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            Some((key.trim(), value.trim()))
        })
        .collect()
}

pub fn parse_bootstrap(public: &str, package: &str) -> Result<Bootstrap, String> {
    let public = ini(public);
    let package = ini(package);
    let route = public
        .get("UrlPath")
        .ok_or("UrlPath missing in KuroPublicConfig.ini")?;
    if !safe_component(route) {
        return Err("Invalid bootstrap UrlPath".into());
    }
    let entry_urls: Vec<_> = public
        .get("InternalPrefix")
        .ok_or("InternalPrefix missing")?
        .split(';')
        .filter(|p| !p.is_empty())
        .map(|prefix| build_download_url(prefix, &format!("{route}/index.json")))
        .collect();
    for url in &entry_urls {
        super::validate_url(url)?;
    }
    if entry_urls.is_empty() {
        return Err("No bootstrap URLs found".into());
    }
    let version = package
        .get("PatchVersion")
        .ok_or("PatchVersion missing in ParamsConfig.ini")?;
    validate_version(version)?;
    Ok(Bootstrap {
        entry_urls,
        package_version: version.to_string(),
    })
}

pub async fn discover(
    folder: &Path,
    settings: &AdditionalResourcesSettings,
) -> Result<Bootstrap, String> {
    if !settings.entry_urls.is_empty() {
        let version = settings.package_version.as_deref().ok_or(
            "Set package_version when overriding entry_urls in additional resource settings",
        )?;
        validate_version(version)?;
        for url in &settings.entry_urls {
            super::validate_url(url)?;
        }
        return Ok(Bootstrap {
            entry_urls: settings.entry_urls.clone(),
            package_version: version.to_string(),
        });
    }

    let public_path = "Client/Config/Kuro/KuroPublicConfig.ini";
    let package_path = "Client/Config/Package/ParamsConfig.ini";
    let loose_public = tokio::fs::read_to_string(folder.join(public_path))
        .await
        .ok();
    let loose_package = tokio::fs::read_to_string(folder.join(package_path))
        .await
        .ok();
    if let (Some(public), Some(package)) = (&loose_public, &loose_package) {
        return apply_version(
            parse_bootstrap(public, package)?,
            settings.package_version.as_deref(),
        );
    }

    let pak = folder.join("Client/Content/Paks/pakchunk0-WindowsNoEditor.pak");
    if !pak.is_file() {
        return Err("Base pakchunk0 is missing. Select an installed Windows game directory, finish the launcher download, or configure entry_urls and package_version.".into());
    }
    let executable = settings
        .vrepak
        .as_deref()
        .ok_or("Configure vrepak before extracting bootstrap files")?;
    let keys = settings
        .cached_keys
        .as_ref()
        .ok_or("Load archive AES keys before extracting bootstrap files")?;
    let guid = pak_guid(&pak)?;
    let key = select_key(keys, &guid)?;
    let public = match loose_public {
        Some(text) => text,
        None => get(executable, &key, &pak, public_path).await?,
    };
    let package = match loose_package {
        Some(text) => text,
        None => get(executable, &key, &pak, package_path).await?,
    };
    apply_version(
        parse_bootstrap(&public, &package)?,
        settings.package_version.as_deref(),
    )
}

fn apply_version(mut bootstrap: Bootstrap, version: Option<&str>) -> Result<Bootstrap, String> {
    if let Some(version) = version {
        validate_version(version)?;
        bootstrap.package_version = version.to_string();
    }
    Ok(bootstrap)
}

async fn get(executable: &str, key: &str, pak: &Path, entry: &str) -> Result<String, String> {
    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        .args(["--engine", "wuthering-waves", "--aes-key", key, "get"])
        .arg(pak)
        .arg(entry);
    let result = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .map_err(|_| format!("vrepak timed out reading {entry}"))?
        .map_err(|e| {
            format!("Cannot run vrepak: {e}. Set vrepak in additional-resources-settings.json or WUWA_VREPAK.")
        })?;
    if !result.status.success() {
        return Err(format!(
            "vrepak could not read {entry} ({}). Check the keys and the vrepak engine profile.",
            result.status
        ));
    }
    String::from_utf8(result.stdout)
        .map_err(|_| format!("vrepak returned invalid UTF-8 for {entry}"))
}

fn select_key(keys: &Value, guid: &str) -> Result<String, String> {
    let key = if guid == "00000000000000000000000000000000" {
        keys.get("mainKey").and_then(Value::as_str)
    } else {
        keys.get("dynamicKeys")
            .and_then(Value::as_array)
            .and_then(|entries| {
                entries.iter().find_map(|entry| {
                    let candidate = entry.get("guid")?.as_str()?.replace('-', "").to_uppercase();
                    (candidate == guid)
                        .then(|| entry.get("key")?.as_str())
                        .flatten()
                })
            })
    }
    .ok_or_else(|| format!("No archive AES key for GUID {guid}; update the keys file"))?;
    let key = key
        .strip_prefix("0x")
        .or_else(|| key.strip_prefix("0X"))
        .unwrap_or(key);
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid archive AES key format".into());
    }
    Ok(format!("0x{key}"))
}

fn pak_guid(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let tail_size = size.min(1024);
    file.seek(SeekFrom::End(-(tail_size as i64)))
        .map_err(|e| e.to_string())?;
    let mut tail = vec![0; tail_size as usize];
    file.read_exact(&mut tail).map_err(|e| e.to_string())?;
    guid_from_tail(&tail, size)
}

fn guid_from_tail(tail: &[u8], file_size: u64) -> Result<String, String> {
    for offset in (17..tail.len().saturating_sub(43)).rev() {
        if tail[offset..offset + 4] != [0xe1, 0x12, 0x6f, 0x5a] {
            continue;
        }
        let version = u32::from_le_bytes(tail[offset + 4..offset + 8].try_into().unwrap());
        let length = tail.len() - offset;
        let valid_length = match version {
            7 => length == 44,
            8 => matches!(length, 172 | 204),
            9 => length == 205,
            10..=12 => length == 204,
            _ => false,
        };
        if !valid_length || tail[offset - 1] > 1 {
            continue;
        }
        let index_offset = u64::from_le_bytes(tail[offset + 8..offset + 16].try_into().unwrap());
        let index_size = u64::from_le_bytes(tail[offset + 16..offset + 24].try_into().unwrap());
        let footer_start = file_size.saturating_sub((tail.len() - offset + 17) as u64);
        if index_offset
            .checked_add(index_size)
            .is_none_or(|end| end > footer_start)
        {
            continue;
        }
        return Ok(tail[offset - 17..offset - 1]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| format!("{:08X}", u32::from_le_bytes(*word)))
            .collect());
    }
    Err("Unsupported or invalid base PAK footer; use a compatible vrepak/game build or explicit bootstrap settings".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loose_ini_bootstrap_needs_no_tools_and_respects_version_override() {
        let root = std::env::temp_dir().join(format!("wuwa-bootstrap-{}", std::process::id()));
        let public = root.join("Client/Config/Kuro");
        let package = root.join("Client/Config/Package");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            public.join("KuroPublicConfig.ini"),
            "UrlPath=bootstrap\nInternalPrefix=https://example.test\n",
        )
        .unwrap();
        std::fs::write(package.join("ParamsConfig.ini"), "PatchVersion=3.8.0\n").unwrap();
        let mut settings = AdditionalResourcesSettings::default();
        let original = discover(&root, &settings).await.unwrap();
        assert_eq!(original.package_version, "3.8.0");
        settings.package_version = Some("3.8.1".into());
        let overridden = discover(&root, &settings).await.unwrap();
        assert_eq!(overridden.package_version, "3.8.1");
        assert_eq!(
            overridden.entry_urls,
            ["https://example.test/bootstrap/index.json"]
        );
        settings.package_version = Some("../invalid".into());
        assert!(discover(&root, &settings).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
