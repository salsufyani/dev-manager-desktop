use std::env::temp_dir;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::Runtime;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::error::Error;

#[tauri::command]
async fn checksum(path: String, algorithm: String) -> Result<String, Error> {
    let mut file = File::open(&path).await?;
    let mut contents: Vec<u8> = vec![];
    file.read_to_end(&mut contents).await?;
    return match algorithm.as_str() {
        "sha256" => Ok(sha256::digest(&contents[..])),
        _ => Err(Error::Unsupported),
    };
}

#[tauri::command]
async fn temp_path(extension: String) -> Result<String, Error> {
    let temp_path = temp_dir().join(format!("webos-dev-tmp-{}{}", Uuid::new_v4(), extension));
    return temp_path
        .to_str()
        .map(|s| String::from(s))
        .ok_or_else(|| Error::new(&format!("Bad temp_path {:?}", temp_path)));
}

pub fn plugin<R: Runtime>(name: &'static str) -> TauriPlugin<R> {
    Builder::new(name)
        .invoke_handler(tauri::generate_handler![checksum, temp_path])
        .build()
}
