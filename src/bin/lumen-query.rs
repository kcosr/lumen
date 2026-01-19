use clap::{Parser, Subcommand};
use serde_json::Value;
use std::error::Error;

#[derive(Parser)]
#[command(name = "lumen-query")]
#[command(about = "Query lumen diff viewer annotations", long_about = None)]
struct Cli {
    /// Lumen API server URL
    #[arg(short, long, default_value = "http://127.0.0.1:7878", env = "LUMEN_API_URL")]
    url: String,

    /// Output format
    #[arg(short, long, default_value = "pretty", value_parser = ["pretty", "json", "compact"])]
    format: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Get current status
    Status,

    /// Get focused hunk details
    CurrentHunk,

    /// Get current file content
    CurrentFile {
        /// Show only new content
        #[arg(long)]
        new_only: bool,

        /// Show only old content
        #[arg(long)]
        old_only: bool,
    },

    /// List annotations
    Annotations {
        /// Show only current file
        #[arg(long)]
        current: bool,
    },

    /// Add annotation to current hunk
    Annotate {
        /// Annotation text
        text: String,
    },

    /// Get full context (status + current hunk + annotations)
    FullContext,

    /// Check if lumen API is running
    Ping,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => {
            let data = get(&cli.url, "/status").await?;
            print_output(&data, &cli.format)?;
        }
        Commands::CurrentHunk => {
            let data = get(&cli.url, "/current-hunk").await?;
            print_output(&data, &cli.format)?;
        }
        Commands::CurrentFile { new_only, old_only } => {
            let data = get(&cli.url, "/current-file").await?;
            if new_only {
                println!("{}", data["new_content"].as_str().unwrap_or(""));
            } else if old_only {
                println!("{}", data["old_content"].as_str().unwrap_or(""));
            } else {
                print_output(&data, &cli.format)?;
            }
        }
        Commands::Annotations { current } => {
            let endpoint = if current {
                "/annotations/current"
            } else {
                "/annotations"
            };
            let data = get(&cli.url, endpoint).await?;
            print_output(&data, &cli.format)?;
        }
        Commands::Annotate { text } => {
            let payload = serde_json::json!({ "content": text });
            let data = post(&cli.url, "/annotation/create", payload).await?;
            print_output(&data, &cli.format)?;
        }
        Commands::FullContext => {
            let data = get(&cli.url, "/full-context").await?;
            print_output(&data, &cli.format)?;
        }
        Commands::Ping => match get(&cli.url, "/status").await {
            Ok(_) => {
                println!("✓ Lumen API is running at {}", cli.url);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("✗ Cannot connect to Lumen API at {}", cli.url);
                eprintln!("  Error: {}", e);
                std::process::exit(1);
            }
        },
    }

    Ok(())
}

async fn get(base_url: &str, endpoint: &str) -> Result<Value, Box<dyn Error>> {
    let url = format!("{}{}", base_url, endpoint);
    let response = reqwest::get(&url).await?;

    if !response.status().is_success() {
        return Err(format!("API error: {}", response.status()).into());
    }

    Ok(response.json().await?)
}

async fn post(base_url: &str, endpoint: &str, payload: Value) -> Result<Value, Box<dyn Error>> {
    let url = format!("{}{}", base_url, endpoint);
    let client = reqwest::Client::new();
    let response = client.post(&url).json(&payload).send().await?;

    if !response.status().is_success() {
        return Err(format!("API error: {}", response.status()).into());
    }

    Ok(response.json().await?)
}

fn print_output(data: &Value, format: &str) -> Result<(), Box<dyn Error>> {
    match format {
        "json" | "compact" => {
            println!("{}", serde_json::to_string(data)?);
        }
        "pretty" | _ => {
            println!("{}", serde_json::to_string_pretty(data)?);
        }
    }
    Ok(())
}
