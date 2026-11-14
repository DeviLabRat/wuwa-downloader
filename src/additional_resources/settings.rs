use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use colored::Colorize;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

use super::{crypto::RemoteCrypto, keys};
use crate::config::status::Status;
use crate::io::util::read_line;

#[derive(Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdditionalResourcesSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vrepak: Option<String>,
    #[serde(alias = "keys_file", skip_serializing_if = "Option::is_none")]
    pub keys_source: Option<String>,
    #[serde(deserialize_with = "expression")]
    pub keys_expression: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_dir: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entry_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<String>,
    #[serde(skip_serializing_if = "default_crypto")]
    pub crypto: RemoteCrypto,
    #[serde(skip)]
    pub cached_keys: Option<Value>,
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
}

fn default_crypto(value: &RemoteCrypto) -> bool {
    *value == RemoteCrypto::default()
}

fn expression<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

impl Default for AdditionalResourcesSettings {
    fn default() -> Self {
        Self {
            vrepak: None,
            keys_source: None,
            keys_expression: keys::DEFAULT_EXPRESSION.into(),
            game_dir: None,
            entry_urls: vec![],
            package_version: None,
            parallel: None,
            crypto: RemoteCrypto::default(),
            cached_keys: None,
            config_path: None,
        }
    }
}

fn absolute(value: &str, base: &Path) -> String {
    let expanded = shellexpand::tilde(value);
    let path = Path::new(expanded.as_ref());
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        base.join(path).to_string_lossy().into_owned()
    }
}

fn question(label: &str, value_label: &str, default: &str) -> Result<String, String> {
    if default.is_empty() {
        print!("{} {label}: ", Status::question());
    } else {
        println!("\n{} {value_label}: {}", Status::info(), default.cyan());
        print!(
            "{} {label} (press Enter to keep the current value): ",
            Status::question()
        );
    }
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let input = read_line().map_err(|e| e.to_string())?;
    if input.is_empty() {
        return Err("Input closed during resource configuration".into());
    }
    let input = input.trim();
    Ok(if input.is_empty() {
        default.to_string()
    } else {
        input.to_string()
    })
}

async fn runnable(executable: &str) -> bool {
    let mut command = Command::new(executable);
    command.arg("--version").kill_on_drop(true);
    matches!(tokio::time::timeout(Duration::from_secs(5), command.output()).await,
        Ok(Ok(output)) if output.status.success())
}

async fn find_vrepak(saved: Option<&str>) -> Option<String> {
    let name = if cfg!(windows) {
        "vrepak.exe"
    } else {
        "vrepak"
    };
    let mut candidates = Vec::new();
    if let Some(path) = saved {
        candidates.push(shellexpand::tilde(path).into_owned());
    }
    if let Ok(path) = std::env::var("WUWA_VREPAK") {
        candidates.push(shellexpand::tilde(&path).into_owned());
    }
    candidates.push(name.to_string());
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        candidates.push(parent.join(name).to_string_lossy().into_owned());
    }
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join(name).to_string_lossy().into_owned());
    }
    for candidate in candidates {
        if runnable(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

fn game_path(value: &str, manual_bootstrap: bool) -> Result<PathBuf, String> {
    let path = PathBuf::from(shellexpand::tilde(value).into_owned());
    if !path.is_dir() {
        return Err("Game directory does not exist".into());
    }
    if !manual_bootstrap
        && !path
            .join("Client/Content/Paks/pakchunk0-WindowsNoEditor.pak")
            .is_file()
        && !(path
            .join("Client/Config/Kuro/KuroPublicConfig.ini")
            .is_file()
            && path
                .join("Client/Config/Package/ParamsConfig.ini")
                .is_file())
    {
        return Err("Select the game root containing Client and its base PAK".into());
    }
    path.canonicalize()
        .map_err(|e| format!("Cannot open game directory: {e}"))
}

impl AdditionalResourcesSettings {
    fn normalize(&mut self) {
        for field in [
            &mut self.vrepak,
            &mut self.keys_source,
            &mut self.game_dir,
            &mut self.package_version,
            &mut self.parallel,
        ] {
            *field = field
                .take()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        self.keys_expression = self.keys_expression.trim().to_string();
        if self.keys_expression.is_empty() {
            self.keys_expression = keys::DEFAULT_EXPRESSION.into();
        }
        self.entry_urls = self
            .entry_urls
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
    }
    pub async fn load() -> Result<Self, String> {
        let explicit = std::env::var("WUWA_ADDITIONAL_RESOURCES_SETTINGS").ok();
        let path = explicit
            .as_deref()
            .unwrap_or("additional-resources-settings.json");
        let path = PathBuf::from(absolute(
            path,
            &std::env::current_dir().map_err(|e| e.to_string())?,
        ));
        match Self::load_from(&path).await {
            Ok(settings) => Ok(settings),
            Err(_) if !path.exists() => Ok(Self {
                config_path: Some(path),
                ..Self::default()
            }),
            Err(error) => Err(error),
        }
    }

    pub async fn load_from(path: &Path) -> Result<Self, String> {
        let path = PathBuf::from(absolute(
            &path.to_string_lossy(),
            &std::env::current_dir().map_err(|e| e.to_string())?,
        ));
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| format!("Cannot read resource settings {}: {e}", path.display()))?;
        let mut settings: Self = if bytes.iter().all(u8::is_ascii_whitespace) {
            Self::default()
        } else {
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid resource settings: {e}"))?;
            if value.is_null() {
                Self::default()
            } else {
                serde_json::from_value(value)
                    .map_err(|e| format!("Invalid resource settings: {e}"))?
            }
        };
        settings.normalize();
        let base = path.parent().unwrap_or(Path::new("."));
        if let Some(source) = &mut settings.keys_source
            && !keys::is_endpoint(source)
        {
            *source = absolute(source, base);
        }
        if let Some(tool) = &mut settings.vrepak
            && tool.contains(['/', '\\'])
        {
            *tool = absolute(tool, base);
        }
        if let Some(game) = &mut settings.game_dir {
            *game = absolute(game, base);
        }
        settings.config_path = Some(path);
        Ok(settings)
    }

    pub async fn save(&self) -> Result<(), String> {
        let path = self
            .config_path
            .as_deref()
            .unwrap_or(Path::new("additional-resources-settings.json"));
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|e| format!("Cannot save resource settings: {e}"))?;
        let result = tokio::fs::rename(&temporary, path).await;
        if let Err(error) = result {
            let _ = tokio::fs::remove_file(temporary).await;
            return Err(format!("Cannot replace resource settings: {error}"));
        }
        println!("{} Configuration saved successfully", Status::success());
        Ok(())
    }

    pub async fn configure(&mut self, client: &Client) -> Result<PathBuf, String> {
        self.normalize();
        println!("\n{} Resource Configuration", Status::info());
        let manual = !self.entry_urls.is_empty() && self.package_version.is_some();
        let folder = loop {
            let default = self.game_dir.as_deref().unwrap_or("");
            let label = if default.is_empty() {
                "Enter the installed game root containing Client (press Enter for the current directory)"
            } else {
                "Enter the installed game root containing Client"
            };
            let input = question(label, "Saved game directory", default)?;
            let input = if input.is_empty() { "." } else { &input };
            match game_path(input, manual) {
                Ok(path) => break path,
                Err(error) => println!("{} {error}", Status::error()),
            }
        };
        let needs_extraction = !manual
            && ![
                "Client/Config/Kuro/KuroPublicConfig.ini",
                "Client/Config/Package/ParamsConfig.ini",
            ]
            .iter()
            .all(|path| folder.join(path).is_file());
        if needs_extraction {
            let found = find_vrepak(self.vrepak.as_deref()).await;
            self.vrepak = if let Some(found) = found {
                println!("{} Found vrepak: {}", Status::success(), found.cyan());
                Some(found)
            } else {
                loop {
                    let input = question("Enter vrepak filepath", "vrepak", "")?;
                    if input.is_empty() {
                        println!("{} vrepak filepath cannot be empty", Status::error());
                        continue;
                    }
                    let executable = if input.contains(['/', '\\']) {
                        absolute(&input, &std::env::current_dir().map_err(|e| e.to_string())?)
                    } else {
                        input
                    };
                    if runnable(&executable).await {
                        break Some(executable);
                    }
                    println!(
                        "{} Failed to run vrepak, please check the filepath",
                        Status::error()
                    );
                }
            };
            loop {
                let default = self
                    .keys_source
                    .clone()
                    .or_else(|| std::env::var("WUWA_KEYS_FILE").ok())
                    .unwrap_or_default();
                let input = question(
                    "Enter keys endpoint URL or local filepath",
                    "Keys source",
                    &default,
                )?;
                if input.is_empty() {
                    println!("{} Keys source cannot be empty", Status::error());
                    continue;
                }
                let source = if keys::is_endpoint(&input) {
                    input
                } else {
                    absolute(&input, &std::env::current_dir().map_err(|e| e.to_string())?)
                };
                let expression = question(
                    "Enter keys expression",
                    "Keys expression",
                    &self.keys_expression,
                )?;
                match keys::load(client, &source, &expression).await {
                    Ok(keys) => {
                        self.keys_source = Some(source);
                        self.keys_expression = expression;
                        self.cached_keys = Some(keys);
                        break;
                    }
                    Err(error) => {
                        println!("{} {error}", Status::error())
                    }
                }
            }
        }
        self.game_dir = Some(folder.to_string_lossy().into_owned());
        self.save().await?;
        Ok(folder)
    }
}
