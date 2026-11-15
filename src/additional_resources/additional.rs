use std::{io::Write, path::Path};

use colored::Colorize;
use reqwest::Client;

use super::{
    AdditionalResourcesSettings, choose_resources, discover_videos, hotfix, resource_context,
};
use crate::config::{
    cfg::{Config, ResourceItem},
    status::Status,
};

struct Content {
    videos: bool,
    core: bool,
}

fn choose_content() -> Result<Option<Content>, String> {
    println!("\n{} Additional Resource Selection", Status::info());
    for (number, label) in [
        (1, "Videos"),
        (2, "Core hotfixes"),
        (3, "Both"),
        (4, "Cancel"),
    ] {
        println!("{} {number}. {label}", Status::question());
    }
    loop {
        print!(
            "\n{} Choose resources to download (1, 2, 3 or 4): ",
            Status::question()
        );
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let input = crate::io::util::read_line().map_err(|e| e.to_string())?;
        if input.is_empty() {
            return Err("Input closed before resource selection".into());
        }
        match input.trim() {
            "1" => {
                return Ok(Some(Content {
                    videos: true,
                    core: false,
                }));
            }
            "2" => {
                return Ok(Some(Content {
                    videos: false,
                    core: true,
                }));
            }
            "3" => {
                return Ok(Some(Content {
                    videos: true,
                    core: true,
                }));
            }
            "4" => return Ok(None),
            _ => println!(
                "{} Invalid choice, please enter 1, 2, 3 or 4",
                Status::error()
            ),
        }
    }
}

pub async fn discover_resources(
    client: &Client,
    folder: &Path,
    settings: &AdditionalResourcesSettings,
) -> Result<Option<(Config, Vec<ResourceItem>)>, String> {
    let context = resource_context(client, folder, settings).await?;
    let content = if super::manifest::supports_videos(&context.package_version)? {
        let Some(content) = choose_content()? else {
            return Ok(None);
        };
        content
    } else {
        Content {
            videos: false,
            core: true,
        }
    };
    let mut resources = Vec::new();
    let mut quality = None;
    if content.core {
        let plan = hotfix::discover_core(client, &mut quality, settings, &context).await?;
        for (group, version) in &plan.versions {
            println!("{} {group}: {}", Status::info(), version.cyan());
        }
        resources.extend(plan.resources);
    }
    let core_files = resources.len();
    if content.videos {
        let plan = discover_videos(client, &mut quality, settings, &context).await?;
        let Some(videos) = choose_resources(&plan)? else {
            return Ok(None);
        };
        resources.extend(videos);
    }
    println!("\n{} Summary", Status::info());
    for (label, selected, items) in [
        ("Core hotfixes", content.core, &resources[..core_files]),
        ("Videos", content.videos, &resources[core_files..]),
    ] {
        if selected {
            let bytes: u64 = items.iter().filter_map(|item| item.size).sum();
            println!(
                "{} {label}: {} files ({:.2} GiB)",
                Status::info(),
                items.len().to_string().cyan(),
                bytes as f64 / 1024_f64.powi(3)
            );
        }
    }
    Ok(Some((context.config(), resources)))
}
