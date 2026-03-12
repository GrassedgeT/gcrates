use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const DEFAULT_DUMP_URL: &str = "https://static.crates.io/db-dump.tar.gz";

pub fn download_and_extract(url: &str, target_dir: &Path) -> Result<()> {
    if target_dir.exists() {
        fs::remove_dir_all(target_dir).with_context(|| {
            format!(
                "failed to remove existing dump directory {}",
                target_dir.display()
            )
        })?;
    }

    let response =
        reqwest::blocking::get(url).with_context(|| format!("failed to download {url}"))?;

    if !response.status().is_success() {
        bail!("failed to download {url}: {}", response.status());
    }

    let tar = flate2::read::GzDecoder::new(response);
    let mut archive = tar::Archive::new(tar);

    for entry in archive.entries().context("failed to enumerate archive entries")? {
        let mut entry = entry.context("failed to read archive entry")?;
        let path = entry.path().context("failed to read archive entry path")?;
        let mut components = path.components();
        if components.next().is_none() {
            continue;
        }

        let stripped_path: PathBuf = components.collect();
        if stripped_path.as_os_str().is_empty() {
            continue;
        }

        let out_path = target_dir.join(&stripped_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create extraction directory {}", parent.display())
            })?;
        }

        entry.unpack(&out_path).with_context(|| {
            format!(
                "failed to extract archive entry to {}",
                out_path.display()
            )
        })?;
    }

    Ok(())
}
