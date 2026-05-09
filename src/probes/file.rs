use std::time::SystemTime;

use tokio::io::AsyncReadExt;

use crate::{
    cli::FileArgs,
    diagnostic,
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport},
};

pub async fn run(
    options: ProbeOptions,
    args: &FileArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    let timeout = options.timeout;
    match tokio::time::timeout(timeout, run_inner(options, args, started)).await {
        Ok(result) => result,
        Err(_) => Err(AppError::failure(format!(
            "file probe timed out after {}",
            humantime::format_duration(timeout)
        ))),
    }
}

async fn run_inner(
    options: ProbeOptions,
    args: &FileArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    if args.path.as_os_str().is_empty() {
        return Err(AppError::invalid_config("--path must not be empty"));
    }
    let path_label = diagnostic::path(&args.path);

    if let (Some(min_size), Some(max_size)) = (args.min_size, args.max_size)
        && min_size > max_size
    {
        return Err(AppError::invalid_config(format!(
            "invalid file size range: min-size {} is greater than max-size {}",
            min_size, max_size
        )));
    }

    if args.max_read == 0 && !args.contains.is_empty() {
        return Err(AppError::invalid_config(
            "--max-read must be greater than 0 when file content assertions are used",
        ));
    }

    if args.contains.iter().any(String::is_empty) {
        return Err(AppError::invalid_config("--contains must not be empty"));
    }

    let metadata = tokio::fs::metadata(&args.path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                AppError::failure(format!("file {path_label} does not exist"))
            }
            _ => AppError::failure(format!("failed to read metadata for {path_label}: {error}")),
        })?;

    if !metadata.is_file() {
        return Err(AppError::failure(format!(
            "{path_label} is not a regular file"
        )));
    }

    if let Some(min_size) = args.min_size
        && metadata.len() < min_size
    {
        return Err(AppError::failure(format!(
            "file {path_label} is smaller than {min_size} bytes"
        )));
    }

    if let Some(max_size) = args.max_size
        && metadata.len() > max_size
    {
        return Err(AppError::failure(format!(
            "file {path_label} is larger than {max_size} bytes"
        )));
    }

    if args.non_empty && metadata.len() == 0 {
        return Err(AppError::failure(format!("file {path_label} is empty")));
    }

    if let Some(max_age) = args.max_age {
        let modified = metadata.modified().map_err(|error| {
            AppError::failure(format!(
                "failed to read modification time for {path_label}: {error}"
            ))
        })?;
        let age = SystemTime::now().duration_since(modified).map_err(|_| {
            AppError::failure(format!(
                "file {path_label} has a modification time in the future"
            ))
        })?;
        if age > max_age {
            return Err(AppError::failure(format!(
                "file {path_label} is older than {}",
                humantime::format_duration(max_age)
            )));
        }
    }

    if args.readable || !args.contains.is_empty() {
        let mut file = tokio::fs::File::open(&args.path)
            .await
            .map_err(|error| AppError::failure(format!("failed to open {path_label}: {error}")))?;

        if !args.contains.is_empty() {
            let file_body = read_file(&mut file, &args.path, args.max_read, &args.contains).await?;
            for needle in &args.contains {
                if !contains_bytes(&file_body.bytes, needle) {
                    if file_body.truncated {
                        return Err(AppError::failure(format!(
                            "file {} was truncated at {} bytes, cannot prove required text {:?}",
                            path_label, args.max_read, needle
                        )));
                    }
                    return Err(AppError::failure(format!(
                        "file {path_label} does not contain required text {needle:?}"
                    )));
                }
            }
        }
    }

    Ok(ProbeReport::new(
        "file",
        args.path.display().to_string(),
        Some(format!("size={}B", metadata.len())),
        started,
        options,
    ))
}

struct BufferedFile {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_file(
    file: &mut tokio::fs::File,
    path: &std::path::Path,
    limit: usize,
    required_texts: &[String],
) -> Result<BufferedFile> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;

    loop {
        let read = file.read(&mut buffer).await.map_err(|error| {
            AppError::failure(format!(
                "failed to read {}: {error}",
                diagnostic::path(path)
            ))
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

        if (!required_texts.is_empty() && contains_all(&bytes, required_texts)) || truncated {
            break;
        }
    }

    Ok(BufferedFile { bytes, truncated })
}

fn contains_all(bytes: &[u8], required_texts: &[String]) -> bool {
    required_texts
        .iter()
        .all(|needle| contains_bytes(bytes, needle))
}

fn contains_bytes(bytes: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    needle.is_empty() || bytes.windows(needle.len()).any(|window| window == needle)
}
