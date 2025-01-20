/// Spin up a web server and test downloading stuff from it.

use std::env;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::process::Command;
use axum::{
    Router,
    routing::get,
    http::HeaderMap,
};

#[tokio::test]
async fn test_download_with_caching() {
    // Override cache directory to point to a temporary directory.
    let cache_dir: TempDir = TempDir::new().unwrap();
    env::set_var("RGET_CACHE_DIR", cache_dir.path());

    // Temporary destination directory.
    let downloads_dir = TempDir::new().unwrap();

    // Set up the test server
    let etag_counter = Arc::new(AtomicUsize::new(0));

    // Create router with our test routes
    let app = Router::new()
        .route("/test.txt", get({
            let etag_counter = etag_counter.clone();
            move || async move {
                let counter = etag_counter.load(Ordering::SeqCst);
                let etag = format!("test_etag{}", counter);
                let mut headers = HeaderMap::new();
                headers.insert("ETag", etag.parse().unwrap());
                (headers, format!("Response data with etag {etag}."))
            }
        }))
        .route("/no_etag.txt", get(|| async { "Response data with no etag." }));

    // Start local server on a random port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base_url = format!("http://{}", addr);

    // Test 1: First download with empty cache
    let output_file = downloads_dir.path().join("output1.txt");
    let status = Command::new(env!("CARGO_BIN_EXE_rget"))
        .arg("--cache")
        .arg(format!("{base_url}/test.txt"))
        .arg("--destination")
        .arg(&output_file)
        .status()
        .await
        .unwrap();

    assert!(status.success());
    assert_eq!(fs::read_to_string(&output_file).unwrap(), "Response data with etag test_etag0.");

    // Test 2: Second download with cache (should use cached version)
    let output_file = downloads_dir.path().join("output2.txt");
    let status = Command::new(env!("CARGO_BIN_EXE_rget"))
        .arg("--cache")
        .arg(format!("{}/test.txt", base_url))
        .arg("--destination")
        .arg(&output_file)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&output_file).unwrap(), "Response data with etag test_etag0.");

    // Test 3: Download without cache flag (should not use or update cache)
    let output_file = downloads_dir.path().join("output3.txt");
    let status = Command::new(env!("CARGO_BIN_EXE_rget"))
        .arg(format!("{}/test.txt", base_url))
        .arg("--destination")
        .arg(&output_file)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&output_file).unwrap(), "Response data with etag test_etag0.");

    // Test 4: Change etag and download with cache. Should get new file.
    etag_counter.fetch_add(1, Ordering::SeqCst);
    let output_file = downloads_dir.path().join("output4.txt");
    let status = Command::new(env!("CARGO_BIN_EXE_rget"))
        .arg("--cache")
        .arg(format!("{}/test.txt", base_url))
        .arg("--destination")
        .arg(&output_file)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&output_file).unwrap(), "Response data with etag test_etag1.");

    // Test 5: Download file without ETag (should still work but not cache)
    let output_file = downloads_dir.path().join("output5.txt");
    let status = Command::new(env!("CARGO_BIN_EXE_rget"))
        .arg("--cache")
        .arg(format!("{}/no_etag.txt", base_url))
        .arg("--destination")
        .arg(&output_file)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&output_file).unwrap(), "Response data with no etag.");

    // Stop the server
    server_handle.abort();
}

// TODO: Test gzip unzipping.
