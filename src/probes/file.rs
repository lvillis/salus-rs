use std::time::SystemTime;

use tokio::io::AsyncReadExt;

use crate::{
    capture::CaptureBuffer,
    cli::FileArgs,
    diagnostic,
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport, with_probe_timeout},
    text_match::{TextMatcher, contains_bytes},
    validation::{
        validate_capture_limit, validate_non_empty_path, validate_non_empty_values,
        validate_positive_duration,
    },
};

pub async fn run(
    options: ProbeOptions,
    args: &FileArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    validate_file_args(args)?;
    with_probe_timeout("file", options.timeout, run_inner(options, args, started)).await
}

fn validate_file_args(args: &FileArgs) -> Result<()> {
    validate_non_empty_path("--path", &args.path)?;

    if let (Some(min_size), Some(max_size)) = (args.min_size, args.max_size)
        && min_size > max_size
    {
        return Err(AppError::invalid_config(format!(
            "invalid file size range: min-size {} is greater than max-size {}",
            min_size, max_size
        )));
    }

    validate_non_empty_values("--contains", &args.contains)?;
    validate_capture_limit(
        "--max-read",
        args.max_read,
        "file content assertions",
        !args.contains.is_empty(),
    )?;

    if let Some(max_age) = args.max_age {
        validate_positive_duration("--max-age", max_age)?;
    }

    Ok(())
}

async fn run_inner(
    options: ProbeOptions,
    args: &FileArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    let path_label = diagnostic::path(&args.path);

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
                if !contains_bytes(file_body.bytes(), needle) {
                    if file_body.is_incomplete() {
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
        diagnostic::path_field(&args.path),
        Some(format!("size={}B", metadata.len())),
        started,
        options,
    ))
}

async fn read_file(
    file: &mut tokio::fs::File,
    path: &std::path::Path,
    limit: usize,
    required_texts: &[String],
) -> Result<CaptureBuffer> {
    let mut body = CaptureBuffer::default();
    let mut buffer = [0_u8; 4096];
    let mut matcher = TextMatcher::new(required_texts);

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

        let previous_len = body.append_limited(&buffer[..read], limit);
        matcher.observe_appended(body.bytes(), previous_len);

        if (!required_texts.is_empty() && matcher.all_matched()) || body.is_incomplete() {
            break;
        }
    }

    Ok(body)
}
