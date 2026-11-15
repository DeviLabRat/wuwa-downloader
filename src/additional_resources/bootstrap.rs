use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

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
    let public_path = "Client/Config/Kuro/KuroPublicConfig.ini";
    let package_path = "Client/Config/Package/ParamsConfig.ini";
    let loose_public = tokio::fs::read_to_string(folder.join(public_path))
        .await
        .ok();
    let loose_package = tokio::fs::read_to_string(folder.join(package_path))
        .await
        .ok();
    if let (Some(public), Some(package)) = (&loose_public, &loose_package) {
        return parse_bootstrap(public, package);
    }

    let archives = archives(folder)?;
    let executable = settings
        .vrepak
        .as_deref()
        .ok_or("Configure vrepak before extracting bootstrap files")?;
    let key = settings
        .main_key
        .as_deref()
        .ok_or("Enter the main AES key before extracting bootstrap files")?;
    let public = match loose_public {
        Some(text) => text,
        None => read_entry(executable, key, &archives, public_path).await?,
    };
    let package = match loose_package {
        Some(text) => text,
        None => read_entry(executable, key, &archives, package_path).await?,
    };
    parse_bootstrap(&public, &package)
}

fn archives(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let directory = folder.join("Client/Content/Paks");
    let entries = std::fs::read_dir(&directory).map_err(|e| {
        format!(
            "Cannot read installed PAK directory {}: {e}",
            directory.display()
        )
    })?;
    let mut archives = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("Cannot read PAK directory entry: {e}"))?
            .path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pak"))
        {
            archives.push(path);
        }
    }
    archives.sort();
    if archives.is_empty() {
        return Err("No installed PAK archives found in Client/Content/Paks".into());
    }
    Ok(archives)
}

async fn read_entry(
    executable: &str,
    key: &str,
    archives: &[PathBuf],
    entry: &str,
) -> Result<String, String> {
    let mut last_error = String::new();
    for pak in archives {
        match get(executable, key, pak, entry).await {
            Ok(text) => return Ok(text),
            Err(error) => last_error = error,
        }
    }
    Err(format!(
        "Cannot read {entry} from installed PAKs. Check the main AES key. {last_error}"
    ))
}

async fn get(executable: &str, key: &str, pak: &Path, entry: &str) -> Result<String, String> {
    let mut command = Command::new(executable);
    command
        .kill_on_drop(true)
        .args(["--engine", "wuthering-waves"]);
    command.args(["--aes-key", key]);
    command.arg("get").arg(pak).arg(entry);
    let result = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .map_err(|_| format!("vrepak timed out reading {entry}"))?
        .map_err(|e| {
            format!(
                "Cannot run vrepak: {e}. Set WUWA_VREPAK or enter the vrepak path when prompted."
            )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loose_ini_bootstrap_needs_no_tools() {
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
        let settings = AdditionalResourcesSettings::default();
        let original = discover(&root, &settings).await.unwrap();
        assert_eq!(original.package_version, "3.8.0");
        assert_eq!(
            original.entry_urls,
            ["https://example.test/bootstrap/index.json"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
