use bytes::Bytes;
use env_logger::{Builder, Env};
use indicatif::{ProgressBar, ProgressStyle};
use log::{info, warn};
use std::env;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::SystemTime;
use std::{fs::create_dir_all, path::PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use cache::CacheEntry;
use clap::Parser;
use futures::{Stream, TryStreamExt};
use tokio::io::AsyncBufRead;

use fork_stream::StreamExt;
use tokio::join;

mod cache;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// If set, cache the download based on etags.
    #[arg(long)]
    cache: bool,

    /// If true the file will be extracted. .tar.gz is assumed.
    #[arg(long)]
    extract: bool,

    /// Destination path.
    #[arg(long)]
    destination: PathBuf,

    /// Don't print a progress bar and set the default log level
    /// to 'warn'. This can be overridden via RGET_LOG=off/trace/debug/info/warn/error
    #[arg(long)]
    quiet: bool,

    /// URL to download.
    url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli: Cli = Cli::parse();

    let default_level = if cli.quiet { "warn" } else { "info" };
    let env = Env::new().filter_or("RGET_LOG", default_level).write_style("RGET_LOG_STYLE");
    Builder::from_env(env)
        .format_timestamp(None)
        .format_target(false)
        .init();

    let save_to: Option<&Path> = (!cli.extract).then_some(&cli.destination);
    let extract_to: Option<&Path> = cli.extract.then_some(&cli.destination);

    let progress = !cli.quiet;

    if cli.cache {
        download_cache(&cli.url, save_to, extract_to, progress).await
    } else {
        download_no_cache(&cli.url, save_to, extract_to, progress).await
    }
}

async fn download_no_cache(
    url: &str,
    save_to: Option<&Path>,
    extract_to: Option<&Path>,
    progress: bool,
) -> Result<()> {
    let response = reqwest::get(url)
        .await
        .with_context(|| anyhow!("GET '{url}'"))?;
    download_response(response, save_to, extract_to, progress).await?;
    Ok(())
}

fn get_cache_dir() -> Option<PathBuf> {
    if let Ok(cache_dir) = env::var("RGET_CACHE_DIR") {
        Some(cache_dir.into())
    } else {
        dirs::cache_dir().or_else(|| dirs::home_dir())
    }
}

async fn download_cache(
    url: &str,
    save_to: Option<&Path>,
    extract_to: Option<&Path>,
    progress: bool,
) -> Result<()> {
    let Some(mut cache_dir) = get_cache_dir() else {
        warn!("Could not determine cache directory; not using cache.");
        return download_no_cache(url, save_to, extract_to, progress).await;
    };

    cache_dir.push("rget");

    info!("Using cache dir: {}", cache_dir.display());

    create_dir_all(&cache_dir)
        .with_context(|| anyhow!("creating cache dir '{}'", cache_dir.display()))?;

    let mut cache = cache::Cache::open(&cache_dir)
        .with_context(|| anyhow!("opening cache index '{}'", cache_dir.display()))?;

    let response = reqwest::get(url)
        .await
        .with_context(|| anyhow!("GET '{url}'"))?;
    let Some(etag) = response.headers().get("etag") else {
        info!("Server did not send etag; cannot cache");
        download_response(response, save_to, extract_to, progress).await?;
        return Ok(());
    };

    let etag = Vec::from(etag.as_bytes());

    let cache_entry = cache
        .get(url)
        .with_context(|| anyhow!("Cache lookup for '{url}'"))?;
    if let Some(cache_entry) = cache_entry {
        if cache_entry.etag == etag {
            // Etag matched! We can just copy the file, assuming it's still there and has the right size.
            // If not, delete it and then proceed as normal.
            match std::fs::metadata(&cache_entry.file_path) {
                Ok(metadata)
                    if metadata.file_type().is_file() && metadata.len() == cache_entry.size =>
                {
                    // All good!
                    info!("Using cached download");

                    // Note there is a minor TOCTOU issue here, if the file changes after
                    // checking it, but filesystem APIs don't allow a solution (files can
                    // always change under you) and in general it should be ok.

                    // Copy the file to the destination or extract it. These should never be set
                    // simultaneously so we don't need to do them in parallel.
                    if let Some(save_to) = save_to {
                        std::fs::copy(&cache_entry.file_path, save_to).with_context(|| {
                            anyhow!(
                                "Copy '{}' to '{}'",
                                cache_entry.file_path.display(),
                                save_to.display()
                            )
                        })?;
                    }
                    if let Some(extract_to) = extract_to {
                        let file =
                            std::fs::File::open(&cache_entry.file_path).with_context(|| {
                                anyhow!("Opening '{}'", cache_entry.file_path.display())
                            })?;
                        let reader = BufReader::new(file);
                        extract(extract_to, reader)?;
                    }
                    return Ok(());
                }
                _ => {
                    // Wrong size or some other error., delete the file and the cache entry.
                    let _ = std::fs::remove_file(cache_entry.file_path);
                    cache
                        .delete(url)
                        .with_context(|| anyhow!("Delete cache entry for '{url}'"))?;
                }
            }
        }
    }

    let cached_file_path = cache_dir.join(unique_file_name());

    // Download the file to the cache, and extract it simultaneously if desired.
    let size = download_response(response, Some(&cached_file_path), extract_to, progress)
        .await
        .context("Reading response")?;

    // Copy it to the destination.
    if let Some(save_to) = save_to {
        info!("Copying to destination");
        std::fs::copy(&cached_file_path, save_to).with_context(|| {
            anyhow!(
                "Copy {} to {}",
                cached_file_path.display(),
                save_to.display()
            )
        })?;
    }

    // Insert it into the cache.
    cache
        .set(
            url,
            &CacheEntry {
                etag,
                size,
                file_path: cached_file_path.to_owned(),
            },
        )
        .with_context(|| anyhow!("Insert cache entry for '{url}'"))?;

    Ok(())
}

/// Download a response to a file, or extract it directly (assumes tar.gz).
async fn download_response(
    response: reqwest::Response,
    save_to: Option<&Path>,
    extract_to: Option<&Path>,
    progress: bool,
) -> Result<u64> {

    let content_length = response.content_length();

    let downloaded_bytes: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let downloaded_bytes_copy = downloaded_bytes.clone();

    let bytes_stream = response
        .bytes_stream()
        // Make the errors cloneable so they can be forked.
        .inspect_ok(move |bytes| { downloaded_bytes_copy.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::AcqRel); } )
        .map_err(|e| Arc::new(e));

    // This is a bit annoying, but we can't conditionally add `inspect_ok`
    // because then we have to Box the stream, and that loses the Sized
    // trait which is required by .fork(). So instead we always inspect
    // and use Box on the inspection closure so it is the same type.
    let progress_inspector: Box<dyn Fn(&Bytes) + Send> = if progress {
        let bar = match content_length {
            Some(length) => ProgressBar::new(length),
            None => ProgressBar::no_length(),
        };
        bar.set_style(ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} [{eta_precise}] {decimal_bytes:>10} / {decimal_total_bytes} {decimal_bytes_per_sec:>10} {msg}").expect("invalid format string"));
        Box::new(move |bytes: &Bytes| bar.inc(bytes.len() as u64))
    } else {
        Box::new(|_: &Bytes| {})
    };

    let bytes_stream = bytes_stream.inspect_ok(progress_inspector);

    match (save_to, extract_to) {
        (Some(save_to), Some(extract_to)) => {
            info!(
                "Downloading to {} and extracting to {}",
                save_to.display(),
                extract_to.display()
            );
            let stream_0 = bytes_stream.fork();
            let stream_1 = stream_0.clone();
            let fut_0 = extract_async(extract_to, to_async_read(stream_0));
            let fut_1 = save_async(save_to, to_async_read(stream_1));
            let (res_0, res_1) = join!(fut_0, fut_1);
            res_0?;
            res_1?;
        }
        (None, Some(extract_to)) => extract_async(extract_to, to_async_read(bytes_stream)).await?,
        (Some(save_to), None) => save_async(save_to, to_async_read(bytes_stream)).await?,
        (None, None) => panic!("Internal error"),
    };

    let downloaded_bytes = downloaded_bytes.load(std::sync::atomic::Ordering::Acquire);

    if let Some(content_length) = content_length {
        if downloaded_bytes != content_length {
            bail!("Content length from server was {content_length} but we downloaded {downloaded_bytes} bytes");
        }
    }

    Ok(downloaded_bytes)
}

fn to_async_read(
    stream: impl Stream<Item = std::result::Result<tokio_util::bytes::Bytes, Arc<reqwest::Error>>>,
) -> impl AsyncBufRead {
    // Map Arc<reqwest::Error> back to io::Error, and wrap with StreamReader.
    tokio_util::io::StreamReader::new(stream.map_err(|ae| std::io::Error::other(ae)))
}

async fn save_async(save_to: &Path, mut stream_reader: impl AsyncBufRead + Unpin) -> Result<()> {
    let mut file = tokio::fs::File::create(&save_to)
        .await
        .with_context(|| anyhow!("Creating destination file '{}'", save_to.display()))?;
    tokio::io::copy(&mut stream_reader, &mut file)
        .await
        .with_context(|| anyhow!("Writing to destination file: '{}'", save_to.display()))?;
    Ok(())
}

async fn extract_async(
    extract_to: &Path,
    async_reader: impl AsyncBufRead + Unpin + Send + 'static,
) -> Result<()> {
    let extract_to = extract_to.to_owned();
    // You can only use SyncIoBridge in a separate thread.
    tokio::task::spawn_blocking(move || -> Result<()> {
        let sync_reader = tokio_util::io::SyncIoBridge::new(async_reader);
        extract(&extract_to, sync_reader)?;
        Ok(())
    })
    .await
    .with_context(|| anyhow!("Tokio task"))?
}

fn extract(extract_to: &Path, reader: impl BufRead) -> Result<()> {
    let tar = flate2::bufread::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(tar);
    archive
        .unpack(extract_to)
        .with_context(|| anyhow!("Decompressing tarball"))?;
    Ok(())
}

/// Return a unique filename by using the current time with high resolution and the process ID.
/// This is used for finding a name to save cached downloads to.
fn unique_file_name() -> String {
    let duration_since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let timestamp_nanos = duration_since_epoch.as_nanos();

    let pid = std::process::id();

    format!("_{:x}_{:x}", timestamp_nanos, pid)
}
