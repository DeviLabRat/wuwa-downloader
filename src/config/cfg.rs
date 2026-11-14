#[derive(Clone)]
pub struct Config {
    pub index_urls: Vec<String>,
    pub zip_bases: Vec<String>,
}

#[derive(Clone)]
pub struct DownloadOptions {
    pub verify_concurrency: usize,
    pub download_concurrency: usize,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            verify_concurrency: 8,
            download_concurrency: 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResourceItem {
    pub dest: String,
    pub remote_path: Option<String>,
    pub checksum: Option<Checksum>,
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub enum Checksum {
    Md5(String),
    Sha1(String),
}

impl Checksum {
    pub fn expected(&self) -> &str {
        match self {
            Self::Md5(value) | Self::Sha1(value) => value,
        }
    }
}
