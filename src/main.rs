use anyhow::{anyhow};
use clap::{self, Parser, command};
use reqwest;
use std::sync::Arc;
use tokio;
use url::Url;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
#[derive(clap::Parser)]
#[command(name = "rget")]
#[command(author = "DCRlike")]
#[command(version = "0.1")]
#[command(about = "A simple Rust-based wget alternative", long_about = None)]
struct Cli {
    /// The URL to download
    url: String,

    /// The path to save the downloaded file
    #[arg(short, long)]
    savepath: Option<String>,

    /// Hash verification
    #[arg(short, long)]
    hash: Option<String>,

    /// Number of threads to use for downloading
    #[arg(short, long, default_value_t = 4)]
    threads: u8,
}

fn get_filename_from_url(url: &str) -> Option<String> {
    let parsed_url = Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed_url.path_segments()?.collect();
    segments.last().map(|s| s.to_string())
}

async fn download_file(url: &String, savepath: &String, num_threads: u8) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    
    // 首先使用HEAD请求检查文件信息
    let head_response = client.head(url).send().await?;
    
    if !head_response.status().is_success() {
        return Err(anyhow!(
            "Failed to connect to server: HTTP {}",
            head_response.status()
        ));
    }

    // 尝试获取Content-Length
    let total_size_opt = head_response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    // 如果没有Content-Length，使用单线程下载
    if total_size_opt.is_none() {
        println!("Server doesn't provide content-length, using single-threaded download");
        return download_single_threaded(url, savepath).await;
    }

    let total_size = total_size_opt.unwrap();

    // 检查服务器是否支持范围请求
    let supports_ranges = head_response
        .headers()
        .get(reqwest::header::ACCEPT_RANGES)
        .map(|v| v.to_str().unwrap_or("").contains("bytes"))
        .unwrap_or(false);

    println!("File size: {} bytes", total_size);
    println!("Server supports range requests: {}", supports_ranges);

    // 如果服务器不支持范围请求或者文件太小，就使用单线程下载
    if !supports_ranges || total_size < 1024 * 1024 || num_threads == 1 {
        println!("Using single-threaded download");
        return download_single_threaded(url, savepath).await;
    }

    let chunk_size = (total_size + num_threads as u64 - 1) / num_threads as u64;
    println!("Using {}-threaded download with chunk size: {} bytes", num_threads, chunk_size);

    // 创建进度条
    let progress_bar = ProgressBar::new(total_size);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta})")
            .unwrap()
            .progress_chars("##-")
    );

    // divide task into num_threads part
    let mut handlers = Vec::new();

    // for all file, open it with Mutex
    let file = Arc::new(tokio::sync::Mutex::new(
        tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(savepath)
            .await?,
    ));

    for i in 0..num_threads {
        let start = i as u64 * chunk_size;
        let end = ((i + 1) as u64 * chunk_size).min(total_size) - 1; // why -1
        let client = client.clone();
        let url = url.clone();
        let file = file.clone();
        let progress_bar = progress_bar.clone();

        //
        let handler = tokio::task::spawn(async move {
            if start >= total_size {
                return Ok(());
            }

            // 添加重试机制
            let max_retries = 3;
            for retry in 0..max_retries {
                let result = download_chunk_with_retry(&client, &url, &file, start, end, &progress_bar, i).await;
                
                match result {
                    Ok(()) => return Ok(()),
                    Err(e) if retry < max_retries - 1 => {
                        eprintln!("Thread {} retry {}: {}", i, retry + 1, e);
                        tokio::time::sleep(tokio::time::Duration::from_millis(1000 * (retry + 1) as u64)).await;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            Ok(()) as anyhow::Result<()>
        });
        
        handlers.push(handler);
    }

    // 等待所有任务完成
    for (i, handler) in handlers.into_iter().enumerate() {
        match handler.await {
            Ok(Ok(())) => println!("Thread {} completed successfully", i),
            Ok(Err(e)) => return Err(anyhow!("Thread {} failed: {}", i, e)),
            Err(e) => return Err(anyhow!("Thread {} panicked: {}", i, e)),
        }
    }

    progress_bar.finish_with_message("Download completed!");
    println!("All downloads completed successfully");
    Ok(())
}

async fn download_single_threaded(url: &String, savepath: &String) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download file: HTTP {}",
            response.status()
        ));
    }

    let total_size = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let mut file = tokio::fs::File::create(savepath).await?;
    let mut stream = response.bytes_stream();

    // 如果知道文件大小，显示进度条
    let progress_bar = if let Some(size) = total_size {
        let pb = ProgressBar::new(size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] {bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, ETA {eta})")
                .unwrap()
                .progress_chars("##-")
        );
        Some(pb)
    } else {
        println!("Downloading... (size unknown)");
        None
    };

    let mut downloaded = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        downloaded += chunk.len() as u64;
        
        if let Some(ref pb) = progress_bar {
            pb.inc(chunk.len() as u64);
        }
    }

    if let Some(pb) = progress_bar {
        pb.finish_with_message("Download completed!");
    } else {
        println!("Download completed! Downloaded {} bytes", downloaded);
    }

    Ok(())
}

async fn download_chunk_with_retry(
    client: &reqwest::Client,
    url: &str,
    file: &Arc<tokio::sync::Mutex<tokio::fs::File>>,
    start: u64,
    end: u64,
    progress_bar: &ProgressBar,
    thread_id: u8,
) -> anyhow::Result<()> {
    let range_header = format!("bytes={}-{}", start, end);
    let resp = client
        .get(url)
        .header(reqwest::header::RANGE, range_header)
        .send()
        .await
        .map_err(|e| anyhow!("Thread {}: Network error: {}", thread_id, e))?;

    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(anyhow!(
            "Thread {}: Server error: HTTP {}",
            thread_id,
            resp.status()
        ));
    }

    // 下载数据并写入文件
    let mut stream = resp.bytes_stream();
    let mut file = file.lock().await;
    
    // 定位到正确的位置
    tokio::io::AsyncSeekExt::seek(&mut *file, std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| anyhow!("Thread {}: File seek error: {}", thread_id, e))?;
    
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|e| anyhow!("Thread {}: Stream error: {}", thread_id, e))?;
        
        tokio::io::AsyncWriteExt::write_all(&mut *file, &chunk)
            .await
            .map_err(|e| anyhow!("Thread {}: File write error: {}", thread_id, e))?;
        
        progress_bar.inc(chunk.len() as u64);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Cli::parse(); // Parse command line arguments

    let url = args.url;
    let path = match args.savepath {
        Some(p) => p,
        None => get_filename_from_url(&url).ok_or_else(|| anyhow!("Could not extract filename from URL"))?,
    };

    println!("Downloading from URL: {}", url);
    println!("Saving to path: {}", path);
    println!("Using {} threads", args.threads);
    
    match download_file(&url, &path, args.threads).await {
        Ok(()) => println!("Download completed successfully."),
        Err(e) => {
            eprintln!("Download failed: {}", e);
            std::process::exit(1);
        }
    }
    
    Ok(())
}
