use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use futures::TryStreamExt;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// If set, cache the download based on etags.
    #[arg(long)]
    cache: bool,

    /// If true the file will be extracted.
    #[arg(long)]
    extract: bool,

    /// Destination path.
    #[arg(long)]
    destination: PathBuf,

    /// URL to download.
    url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli: Cli = Cli::parse();

    let response = reqwest::get(cli.url).await?;

    let bytes_stream = response
        .bytes_stream()
        .map_err(|e| std::io::Error::other(e));
    let stream_reader = tokio_util::io::StreamReader::new(bytes_stream);
    let body_reader = tokio_util::io::SyncIoBridge::new(stream_reader);

    if cli.extract {
        let tar = flate2::bufread::GzDecoder::new(body_reader);
        let mut archive = tar::Archive::new(tar);
        archive.unpack(cli.destination)?;
    } else {
        todo!();
    }

    Ok(())
}
