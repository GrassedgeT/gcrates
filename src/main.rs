use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let url = "https://static.crates.io/db-dump.tar.gz";
    let target_dir = env::current_dir()?.join("db-dump");

    // 解压前如果目标文件夹已经存在，则先删除它
    if target_dir.exists() {
        println!("Removing existing directory {}...", target_dir.display());
        fs::remove_dir_all(&target_dir)?;
    }

    println!("Downloading {}...", url);
    let response = reqwest::blocking::get(url)?;

    if !response.status().is_success() {
        return Err(format!("Failed to download: {}", response.status()).into());
    }

    println!("Extracting to {}...", target_dir.display());

    let tar = flate2::read::GzDecoder::new(response);
    let mut archive = tar::Archive::new(tar);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();

        // 跳过路径的第一个组件（例如 2026-03-06... 文件夹）
        let mut components = path.components();
        if components.next().is_none() {
            continue;
        }

        let stripped_path: PathBuf = components.collect();
        // 如果去除了第一层之后路径为空（说明这个 entry 就是顶层文件夹本身），则跳过
        if stripped_path.as_os_str().is_empty() {
            continue;
        }

        let out_path = target_dir.join(&stripped_path);

        // 确保父目录存在
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // 将条目解压到目标路径
        entry.unpack(&out_path)?;
    }

    println!("Extraction complete.");

    Ok(())
}
