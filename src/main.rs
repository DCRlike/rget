use clap::{self, Parser, command};
use reqwest;
use url::Url;
#[derive(clap::Parser)]
#[command(name = "rget")]
#[command(author = "DCRlike")]
#[command(version = "0.1")]
#[command(about = "A simple Rust-based wget alternative", long_about = None)]
struct Cli {
    /// The URL to download
    url: String,

    /// The path to save the downloaded file
    #[arg(short = 's', long)]
    savepath: Option<String>,
}

fn get_filename_from_url(url: &str) -> Option<String> {
    let parsed_url = Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed_url.path_segments()?.collect();
    segments.last().map(|s| s.to_string())
}

fn main() {
    let args = Cli::parse();
    let url = args.url;
    let path = args.savepath.unwrap_or_else(|| {
        get_filename_from_url(&url).unwrap_or_else(|| {
            eprintln!("Error: Could not determine filename from URL. Please specify a path.");
            std::process::exit(1);
        })
    });

    println!("Downloading from URL: {}", url);
    println!("Saving to path: {}", path);
    let mut response = reqwest::blocking::get(url).expect("Request failed");
    let mut file = std::fs::File::create(&path).expect("Can not create this file");

    std::io::copy(&mut response, &mut file).expect("Failed to copy content to file");
    println!("Download completed successfully.");
}
