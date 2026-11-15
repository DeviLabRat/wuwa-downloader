use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use colored::Colorize;
use tokio::process::Command;

use super::crypto::RemoteCrypto;
use crate::config::status::Status;
use crate::io::{file::get_dir, util::read_line};

#[derive(Default)]
pub struct AdditionalResourcesSettings {
    pub vrepak: Option<String>,
    pub crypto: RemoteCrypto,
    pub main_key: Option<String>,
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

async fn find_vrepak() -> Option<String> {
    let name = if cfg!(windows) {
        "vrepak.exe"
    } else {
        "vrepak"
    };
    let mut candidates = Vec::new();
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

impl AdditionalResourcesSettings {
    pub async fn configure(&mut self) -> Result<PathBuf, String> {
        println!("\n{} Resource Configuration", Status::info());
        let folder = get_dir().map_err(|e| e.to_string())?;
        let needs_extraction = ![
            "Client/Config/Kuro/KuroPublicConfig.ini",
            "Client/Config/Package/ParamsConfig.ini",
        ]
        .iter()
        .all(|path| folder.join(path).is_file());
        if needs_extraction {
            let found = find_vrepak().await;
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
                let key = question("Enter main AES key", "Main AES key", "")?;
                let key = key
                    .strip_prefix("0x")
                    .or_else(|| key.strip_prefix("0X"))
                    .unwrap_or(&key);
                if key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    self.main_key = Some(format!("0x{key}"));
                    break;
                }
                println!(
                    "{} AES keys must contain 64 hexadecimal digits",
                    Status::error()
                );
            }
        }
        Ok(folder)
    }
}
