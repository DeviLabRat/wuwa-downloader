use colored::*;
use reqwest::Client;

#[cfg(not(target_os = "windows"))]
use std::process::Command;
use std::sync::{Arc, atomic::AtomicBool};

#[cfg(windows)]
use winconsole::console::{clear, set_title};

#[cfg(windows)]
fn enable_ansi_support() {
    use std::ffi::c_void;

    unsafe extern "system" {
        fn GetStdHandle(std_handle: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }

    unsafe {
        const STD_OUTPUT_HANDLE: u32 = 0xFFFFFFF5u32 as u32;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if !stdout.is_null() {
            let mut mode: u32 = 0;
            if GetConsoleMode(stdout, &mut mode) != 0 {
                mode |= ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                SetConsoleMode(stdout, mode);
            }
        }
    }
}

use wuwa_downloader::{
    additional_resources::{self, AdditionalResourcesSettings},
    config::status::Status,
    download::pipeline::run_pipeline,
    io::{
        console::print_results,
        file::get_dir,
        logging::setup_logging,
        util::{ask_concurrency, exit_with_error, parse_resources, setup_ctrlc},
    },
    network::client::{ask_download_mode, fetch_index, get_config},
};

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    clear().unwrap();
    #[cfg(not(target_os = "windows"))]
    Command::new("clear").status().unwrap();

    #[cfg(windows)]
    {
        set_title("Wuthering Waves Downloader").unwrap();
        enable_ansi_support();
    }

    let log_file = setup_logging();
    let client = Client::new();

    let mode = match ask_download_mode(&client) {
        Ok(mode) => mode,
        Err(e) => exit_with_error(&log_file, &e),
    };
    let config = if mode != "resources" {
        match get_config(&client, &mode).await {
            Ok(c) => Some(c),
            Err(e) => exit_with_error(&log_file, &e),
        }
    } else {
        None
    };

    let mut additional_resources_settings =
        (mode == "resources").then(AdditionalResourcesSettings::default);
    let directory = if let Some(settings) = &mut additional_resources_settings {
        settings.configure().await
    } else {
        get_dir().map_err(|e| e.to_string())
    };
    let folder = match directory {
        Ok(folder) => folder,
        Err(e) => exit_with_error(&log_file, &format!("Failed to configure download: {}", e)),
    };
    let options = match ask_concurrency() {
        Ok(options) => options,
        Err(e) => exit_with_error(&log_file, &format!("Failed to read concurrency: {}", e)),
    };

    #[cfg(windows)]
    clear().unwrap();
    #[cfg(not(target_os = "windows"))]
    Command::new("clear").status().unwrap();

    println!(
        "\n{} Download folder: {}",
        Status::info(),
        folder.display().to_string().cyan()
    );
    println!(
        "{} Download concurrency: {}",
        Status::info(),
        options.download_concurrency.to_string().cyan()
    );
    println!(
        "{} Verify concurrency: {}\n",
        Status::info(),
        options.verify_concurrency.to_string().cyan()
    );

    let (config, resources) = if let Some(config) = config {
        let mut resources = Vec::new();
        for index_url in &config.index_urls {
            let data = match fetch_index(&client, index_url, &log_file).await {
                Ok(data) => data,
                Err(e) => exit_with_error(&log_file, &e),
            };
            match parse_resources(&data) {
                Ok(items) => resources.extend(items),
                Err(err) => exit_with_error(&log_file, &err),
            }
        }

        (config, resources)
    } else if let Some(settings) = additional_resources_settings {
        match additional_resources::discover_resources(&client, &folder, &settings).await {
            Ok(Some(plan)) => plan,
            Ok(None) => return,
            Err(error) => exit_with_error(&log_file, &error),
        }
    } else {
        unreachable!(
            "download mode must provide either launcher configuration or resource settings"
        );
    };

    println!(
        "{} Found {} files to check and/or download\n",
        Status::info(),
        resources.len().to_string().cyan()
    );
    let should_stop = Arc::new(AtomicBool::new(false));
    setup_ctrlc(should_stop.clone());
    let result = run_pipeline(
        Arc::new(client),
        Arc::new(config),
        resources,
        folder.clone(),
        log_file.clone(),
        should_stop.clone(),
        options,
    )
    .await;

    #[cfg(windows)]
    clear().unwrap();

    print_results(&result, &folder);

    if should_stop.load(std::sync::atomic::Ordering::SeqCst) {
        std::process::exit(130);
    }
}
