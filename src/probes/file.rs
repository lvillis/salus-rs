use std::time::SystemTime;

use tokio::io::AsyncReadExt;

use crate::{
    cli::{Cli, FileArgs},
    error::{AppError, Result},
    probe::ProbeReport,
};

pub async fn run(cli: Cli, args: FileArgs, started: std::time::Instant) -> Result<ProbeReport> {
    if let (Some(min_size), Some(max_size)) = (args.min_size, args.max_size)
        && min_size > max_size
    {
        return Err(AppError::invalid_config(format!(
            "invalid file size range: min-size {} is greater than max-size {}",
            min_size, max_size
        )));
    }

    if args.max_read_bytes == 0 && !args.contains.is_empty() {
        return Err(AppError::invalid_config(
            "--max-read-bytes must be greater than 0 when file content assertions are used",
        ));
    }

    let metadata = tokio::fs::metadata(&args.path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                AppError::failure(format!("file {} does not exist", args.path.display()))
            }
            _ => AppError::failure(format!(
                "failed to read metadata for {}: {error}",
                args.path.display()
            )),
        })?;

    if !metadata.is_file() {
        return Err(AppError::failure(format!(
            "{} is not a regular file",
            args.path.display()
        )));
    }

    if let Some(min_size) = args.min_size
        && metadata.len() < min_size
    {
        return Err(AppError::failure(format!(
            "file {} is smaller than {} bytes",
            args.path.display(),
            min_size
        )));
    }

    if let Some(max_size) = args.max_size
        && metadata.len() > max_size
    {
        return Err(AppError::failure(format!(
            "file {} is larger than {} bytes",
            args.path.display(),
            max_size
        )));
    }

    if args.non_empty && metadata.len() == 0 {
        return Err(AppError::failure(format!(
            "file {} is empty",
            args.path.display()
        )));
    }

    if let Some(max_age) = args.max_age {
        let modified = metadata.modified().map_err(|error| {
            AppError::failure(format!(
                "failed to read modification time for {}: {error}",
                args.path.display()
            ))
        })?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        if age > max_age {
            return Err(AppError::failure(format!(
                "file {} is older than {}",
                args.path.display(),
                humantime::format_duration(max_age)
            )));
        }
    }

    if args.readable || !args.contains.is_empty() {
        let mut file = tokio::fs::File::open(&args.path).await.map_err(|error| {
            AppError::failure(format!("failed to open {}: {error}", args.path.display()))
        })?;

        if !args.contains.is_empty() {
            let file_body = read_file(&mut file, &args.path, args.max_read_bytes).await?;
            let body = String::from_utf8_lossy(&file_body.bytes);
            for needle in &args.contains {
                if !body.contains(needle) {
                    if file_body.truncated {
                        return Err(AppError::failure(format!(
                            "file {} was truncated at {} bytes, cannot prove required text {:?}",
                            args.path.display(),
                            args.max_read_bytes,
                            needle
                        )));
                    }
                    return Err(AppError::failure(format!(
                        "file {} does not contain required text {:?}",
                        args.path.display(),
                        needle
                    )));
                }
            }
        }
    }

    Ok(ProbeReport {
        mode: "file",
        target: args.path.display().to_string(),
        detail: Some(format!("size={}B", metadata.len())),
        started,
        cli,
    })
}

struct BufferedFile {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_file(
    file: &mut tokio::fs::File,
    path: &std::path::Path,
    limit: usize,
) -> Result<BufferedFile> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;

    loop {
        let read = file.read(&mut buffer).await.map_err(|error| {
            AppError::failure(format!("failed to read {}: {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }

        if bytes.len() < limit {
            let remaining = limit - bytes.len();
            if read > remaining {
                truncated = true;
            }
            let slice = &buffer[..read.min(remaining)];
            bytes.extend_from_slice(slice);
        } else {
            truncated = true;
        }

        if truncated {
            break;
        }
    }

    Ok(BufferedFile { bytes, truncated })
}
