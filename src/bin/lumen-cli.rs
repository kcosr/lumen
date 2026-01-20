use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use std::error::Error;

#[derive(Clone, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    JsonPretty,
}

#[derive(Parser)]
#[command(name = "lumen-cli")]
#[command(about = "Query lumen diff viewer annotations", long_about = None)]
struct Cli {
    /// Lumen API server URL
    #[arg(
        short,
        long,
        default_value = "http://127.0.0.1:7878",
        env = "LUMEN_API_URL"
    )]
    url: String,

    /// Output format
    #[arg(short, long, value_enum, default_value = "text")]
    format: OutputFormat,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Get current status
    Status,

    /// Get focused hunk details
    CurrentHunk,

    /// List hunks
    Hunks {
        /// Show only current file
        #[arg(long)]
        current: bool,
    },

    /// Get hunk details by index
    Hunk {
        /// File index from status/hunks output
        file_index: usize,
        /// Hunk index from hunks output
        hunk_index: usize,
    },

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
        /// Target file index
        #[arg(long)]
        file_index: Option<usize>,
        /// Target hunk index
        #[arg(long)]
        hunk_index: Option<usize>,
    },

    /// Update an annotation by id
    Update {
        /// Annotation id
        id: String,
        /// Updated annotation text
        text: String,
    },

    /// Delete an annotation by id
    Delete {
        /// Annotation id
        id: String,
    },

    /// Get full context (status + current hunk + annotations)
    FullContext,

    /// Check if lumen API is running
    Ping,

    /// Tag operations for hunks
    Tag {
        #[command(subcommand)]
        action: TagCommands,
    },

    /// List tags per hunk
    Tags {
        /// Show only current file
        #[arg(long)]
        current: bool,
    },
}

#[derive(Subcommand)]
enum TagCommands {
    /// List available tags in the repo
    List,

    /// Show tags for the focused hunk
    Current,

    /// Set tags for the focused hunk
    Set {
        /// Tags to set on the focused hunk
        #[arg(required = true)]
        tags: Vec<String>,
        /// Target file index
        #[arg(long)]
        file_index: Option<usize>,
        /// Target hunk index
        #[arg(long)]
        hunk_index: Option<usize>,
    },

    /// Add a tag to the focused hunk
    Add {
        /// Tag to add
        tag: String,
        /// Target file index
        #[arg(long)]
        file_index: Option<usize>,
        /// Target hunk index
        #[arg(long)]
        hunk_index: Option<usize>,
    },

    /// Remove a tag from the focused hunk
    Remove {
        /// Tag to remove
        tag: String,
        /// Target file index
        #[arg(long)]
        file_index: Option<usize>,
        /// Target hunk index
        #[arg(long)]
        hunk_index: Option<usize>,
    },

    /// Clear all tags from the focused hunk
    Clear {
        /// Target file index
        #[arg(long)]
        file_index: Option<usize>,
        /// Target hunk index
        #[arg(long)]
        hunk_index: Option<usize>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Status => {
            let data = get(&cli.url, "/status").await?;
            print_output(&data, &cli.format, OutputKind::Status)?;
        }
        Commands::CurrentHunk => {
            let data = get(&cli.url, "/current-hunk").await?;
            print_output(&data, &cli.format, OutputKind::CurrentHunk)?;
        }
        Commands::Hunks { current } => {
            let endpoint = if current { "/hunks/current" } else { "/hunks" };
            let data = get(&cli.url, endpoint).await?;
            print_output(&data, &cli.format, OutputKind::Hunks)?;
        }
        Commands::Hunk {
            file_index,
            hunk_index,
        } => {
            let payload = serde_json::json!({
                "file_index": file_index,
                "hunk_index": hunk_index,
            });
            let data = post(&cli.url, "/hunk", payload).await?;
            print_output(&data, &cli.format, OutputKind::Hunk)?;
        }
        Commands::CurrentFile { new_only, old_only } => {
            let data = get(&cli.url, "/current-file").await?;
            if new_only {
                print_content_only(&data, &cli.format, "new_content")?;
            } else if old_only {
                print_content_only(&data, &cli.format, "old_content")?;
            } else {
                print_output(&data, &cli.format, OutputKind::CurrentFile)?;
            }
        }
        Commands::Annotations { current } => {
            let endpoint = if current {
                "/annotations/current"
            } else {
                "/annotations"
            };
            let data = get(&cli.url, endpoint).await?;
            print_output(&data, &cli.format, OutputKind::Annotations)?;
        }
        Commands::Annotate {
            text,
            file_index,
            hunk_index,
        } => {
            let payload = if let Some((file_index, hunk_index)) =
                target_indices(file_index, hunk_index)?
            {
                serde_json::json!({
                    "file_index": file_index,
                    "hunk_index": hunk_index,
                    "content": text,
                })
            } else {
                serde_json::json!({ "content": text })
            };
            let endpoint = if payload.get("file_index").is_some() {
                "/annotation/create-target"
            } else {
                "/annotation/create"
            };
            let data = post(&cli.url, endpoint, payload).await?;
            print_output(
                &data,
                &cli.format,
                OutputKind::Annotation { action: "created" },
            )?;
        }
        Commands::Update { id, text } => {
            let payload = serde_json::json!({ "id": id, "content": text });
            let data = post(&cli.url, "/annotation/update", payload).await?;
            print_output(
                &data,
                &cli.format,
                OutputKind::Annotation { action: "updated" },
            )?;
        }
        Commands::Delete { id } => {
            let payload = serde_json::json!({ "id": id });
            let data = post(&cli.url, "/annotation/delete", payload).await?;
            print_output(
                &data,
                &cli.format,
                OutputKind::Annotation { action: "deleted" },
            )?;
        }
        Commands::FullContext => {
            let data = get(&cli.url, "/full-context").await?;
            print_output(&data, &cli.format, OutputKind::FullContext)?;
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
        Commands::Tag { action } => match action {
            TagCommands::List => {
                let data = get(&cli.url, "/tags").await?;
                print_output(&data, &cli.format, OutputKind::TagsList)?;
            }
            TagCommands::Current => {
                let data = get(&cli.url, "/tags/current").await?;
                print_output(&data, &cli.format, OutputKind::TagsCurrent)?;
            }
            TagCommands::Set {
                tags,
                file_index,
                hunk_index,
            } => {
                let normalized = normalize_tags(tags);
                let payload = if let Some((file_index, hunk_index)) =
                    target_indices(file_index, hunk_index)?
                {
                    serde_json::json!({
                        "file_index": file_index,
                        "hunk_index": hunk_index,
                        "tags": normalized,
                    })
                } else {
                    serde_json::json!({ "tags": normalized })
                };
                let endpoint = if payload.get("file_index").is_some() {
                    "/tags/set-target"
                } else {
                    "/tags/set"
                };
                let data = post(&cli.url, endpoint, payload).await?;
                print_output(
                    &data,
                    &cli.format,
                    OutputKind::TagsAction { action: "updated" },
                )?;
            }
            TagCommands::Add {
                tag,
                file_index,
                hunk_index,
            } => {
                let target = target_indices(file_index, hunk_index)?;
                let mut tags = if let Some((file_index, hunk_index)) = target {
                    fetch_hunk_tags(&cli.url, file_index, hunk_index).await?
                } else {
                    fetch_current_tags(&cli.url).await?
                };
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
                let payload = if let Some((file_index, hunk_index)) = target {
                    serde_json::json!({
                        "file_index": file_index,
                        "hunk_index": hunk_index,
                        "tags": normalize_tags(tags),
                    })
                } else {
                    serde_json::json!({ "tags": normalize_tags(tags) })
                };
                let endpoint = if payload.get("file_index").is_some() {
                    "/tags/set-target"
                } else {
                    "/tags/set"
                };
                let data = post(&cli.url, endpoint, payload).await?;
                print_output(
                    &data,
                    &cli.format,
                    OutputKind::TagsAction { action: "updated" },
                )?;
            }
            TagCommands::Remove {
                tag,
                file_index,
                hunk_index,
            } => {
                let target = target_indices(file_index, hunk_index)?;
                let mut tags = if let Some((file_index, hunk_index)) = target {
                    fetch_hunk_tags(&cli.url, file_index, hunk_index).await?
                } else {
                    fetch_current_tags(&cli.url).await?
                };
                tags.retain(|t| t != &tag);
                let payload = if let Some((file_index, hunk_index)) = target {
                    serde_json::json!({
                        "file_index": file_index,
                        "hunk_index": hunk_index,
                        "tags": normalize_tags(tags),
                    })
                } else {
                    serde_json::json!({ "tags": normalize_tags(tags) })
                };
                let endpoint = if payload.get("file_index").is_some() {
                    "/tags/set-target"
                } else {
                    "/tags/set"
                };
                let data = post(&cli.url, endpoint, payload).await?;
                print_output(
                    &data,
                    &cli.format,
                    OutputKind::TagsAction { action: "updated" },
                )?;
            }
            TagCommands::Clear {
                file_index,
                hunk_index,
            } => {
                let payload = if let Some((file_index, hunk_index)) =
                    target_indices(file_index, hunk_index)?
                {
                    serde_json::json!({
                        "file_index": file_index,
                        "hunk_index": hunk_index,
                        "tags": [],
                    })
                } else {
                    serde_json::json!({ "tags": [] })
                };
                let endpoint = if payload.get("file_index").is_some() {
                    "/tags/set-target"
                } else {
                    "/tags/set"
                };
                let data = post(&cli.url, endpoint, payload).await?;
                print_output(
                    &data,
                    &cli.format,
                    OutputKind::TagsAction { action: "cleared" },
                )?;
            }
        },
        Commands::Tags { current } => {
            let endpoint = if current { "/hunks/current" } else { "/hunks" };
            let data = get(&cli.url, endpoint).await?;
            let filtered = filter_hunks_with_tags(&data);
            print_output(&filtered, &cli.format, OutputKind::Hunks)?;
        }
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

enum OutputKind {
    Status,
    CurrentHunk,
    CurrentFile,
    Annotations,
    Annotation { action: &'static str },
    Hunks,
    Hunk,
    TagsList,
    TagsCurrent,
    TagsAction { action: &'static str },
    FullContext,
}

fn print_output(
    data: &Value,
    format: &OutputFormat,
    kind: OutputKind,
) -> Result<(), Box<dyn Error>> {
    match format {
        OutputFormat::Text => match kind {
            OutputKind::Status => print_text_status(data),
            OutputKind::CurrentHunk => print_text_current_hunk(data),
            OutputKind::CurrentFile => print_text_current_file(data),
            OutputKind::Annotations => print_text_annotations(data),
            OutputKind::Annotation { action } => print_text_annotation(data, action),
            OutputKind::Hunks => print_text_hunks(data),
            OutputKind::Hunk => print_text_hunk(data),
            OutputKind::TagsList => print_text_tags_list(data),
            OutputKind::TagsCurrent => print_text_tags_current(data),
            OutputKind::TagsAction { action } => print_text_tags_action(data, action),
            OutputKind::FullContext => print_text_full_context(data),
        },
        OutputFormat::Json => print_json(data, false)?,
        OutputFormat::JsonPretty => print_json(data, true)?,
    }
    Ok(())
}

fn print_content_only(
    data: &Value,
    format: &OutputFormat,
    field: &str,
) -> Result<(), Box<dyn Error>> {
    match format {
        OutputFormat::Text => {
            println!("{}", data.get(field).and_then(|v| v.as_str()).unwrap_or(""));
        }
        OutputFormat::Json => {
            let value = data.get(field).cloned().unwrap_or(Value::Null);
            print_json(&value, false)?;
        }
        OutputFormat::JsonPretty => {
            let value = data.get(field).cloned().unwrap_or(Value::Null);
            print_json(&value, true)?;
        }
    }
    Ok(())
}

fn print_json(data: &Value, pretty: bool) -> Result<(), Box<dyn Error>> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(data)?);
    } else {
        println!("{}", serde_json::to_string(data)?);
    }
    Ok(())
}

fn print_text_status(data: &Value) {
    let cwd = data.get("cwd").and_then(|v| v.as_str()).unwrap_or("-");
    let scope = format_scope(data.get("scope"));
    let current_file = data
        .get("current_file")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let current_file_index = data
        .get("current_file_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let focused_hunk = data
        .get("focused_hunk")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let hunk_count = data
        .get("hunk_count")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let annotations_count = data
        .get("annotations_count")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());

    println!("cwd: {}", cwd);
    println!("scope: {}", scope);
    println!("current_file: {} ({})", current_file, current_file_index);
    println!("focused_hunk: {}", focused_hunk);
    println!("hunk_count: {}", hunk_count);
    println!("annotations_count: {}", annotations_count);
}

fn print_text_current_hunk(data: &Value) {
    let file = data.get("file").and_then(|v| v.as_str()).unwrap_or("-");
    let file_index = data
        .get("file_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let hunk_index = data
        .get("hunk_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let line_range = format_range(data.get("line_range"));
    let old_range = format_range(data.get("old_line_range"));
    let new_range = format_range(data.get("new_line_range"));
    let change_type = data
        .get("change_type")
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    println!("file: {} ({})", file, file_index);
    println!("hunk: {}", hunk_index);
    println!("lines: {}", line_range.unwrap_or_else(|| "-".to_string()));
    if old_range.is_some() {
        println!(
            "old_lines: {}",
            old_range.unwrap_or_else(|| "-".to_string())
        );
    }
    if new_range.is_some() {
        println!(
            "new_lines: {}",
            new_range.unwrap_or_else(|| "-".to_string())
        );
    }
    println!("change: {}", change_type);

    let diff_text = data.get("diff_text").and_then(|v| v.as_str()).unwrap_or("");
    if !diff_text.is_empty() {
        println!("\ndiff:\n{}", diff_text);
    }

    print_context_block("context_before", data.get("context_before"));
    print_context_block("context_changed", data.get("context_changed"));
    print_context_block("context_after", data.get("context_after"));

    let tags = extract_tags(data);
    if tags.is_empty() {
        println!("\ntags: none");
    } else {
        println!("\ntags: {}", tags.join(", "));
    }

    let annotation = data.get("annotation").and_then(|v| v.as_object());
    if let Some(annotation) = annotation {
        println!(
            "\nannotation: {} {}",
            annotation.get("id").and_then(|v| v.as_str()).unwrap_or("-"),
            annotation
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        );
    } else {
        println!("\nannotation: none");
    }
}

fn print_text_current_file(data: &Value) {
    let file = data.get("file").and_then(|v| v.as_str()).unwrap_or("-");
    let file_index = data
        .get("file_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("-");
    let is_binary = data
        .get("is_binary")
        .and_then(|v| v.as_bool())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let old_content = data
        .get("old_content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_content = data
        .get("new_content")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    println!("file: {} ({})", file, file_index);
    println!("status: {}", status);
    println!("binary: {}", is_binary);

    if !old_content.is_empty() {
        println!("\nold_content:\n{}", old_content);
    }
    if !new_content.is_empty() {
        println!("\nnew_content:\n{}", new_content);
    }
}

fn print_text_annotations(data: &Value) {
    let annotations = data
        .get("annotations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!("annotations: {}", annotations.len());
    for annotation in annotations {
        if let Some(line) = format_annotation_line(&annotation) {
            println!("- {}", line);
        }
    }
}

fn print_text_annotation(data: &Value, action: &str) {
    let annotation = data.get("annotation").unwrap_or(&Value::Null);
    if let Some(line) = format_annotation_line(annotation) {
        println!("annotation {}: {}", action, line);
    } else {
        println!("annotation {}.", action);
    }
}

fn print_text_hunks(data: &Value) {
    let hunks = data
        .get("hunks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    println!("hunks: {}", hunks.len());
    for hunk in hunks {
        print_text_hunk_line(&hunk);
    }
}

fn print_text_hunk(data: &Value) {
    print_text_hunk_line(data);
    if let Some(diff_text) = data.get("diff_text").and_then(|v| v.as_str()) {
        if !diff_text.is_empty() {
            println!("\n{}", diff_text.trim_end());
        }
    }
}

fn print_text_hunk_line(hunk: &Value) {
    let file = hunk.get("file").and_then(|v| v.as_str()).unwrap_or("-");
    let file_index = hunk
        .get("file_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let hunk_index = hunk
        .get("hunk_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let line_range = format_range(hunk.get("line_range")).unwrap_or_else(|| "-".to_string());
    let change_type = hunk.get("change_type").and_then(|v| v.as_str()).unwrap_or("-");
    let tags = extract_tags(hunk);
    let annotation_id = hunk
        .get("annotation")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let tags_display = if tags.is_empty() {
        "tags: (none)".to_string()
    } else {
        format!("tags: {}", tags.join(", "))
    };
    let annotation_display = if annotation_id.is_empty() {
        "annotation: (none)".to_string()
    } else {
        format!("annotation: {}", annotation_id)
    };

    println!(
        "- {} ({}:{}): {} [{}] {}",
        file, file_index, hunk_index, line_range, change_type, annotation_display
    );
    println!("  {}", tags_display);
}

fn print_text_tags_list(data: &Value) {
    let tags = extract_tags(data);
    println!("tags: {}", tags.len());
    if tags.is_empty() {
        println!("- (none)");
    } else {
        for tag in tags {
            println!("- {}", tag);
        }
    }
}

fn print_text_tags_current(data: &Value) {
    let file = data.get("file").and_then(|v| v.as_str()).unwrap_or("-");
    let file_index = data
        .get("file_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let hunk_index = data
        .get("hunk_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let line_range = format_range(data.get("line_range"));
    let tags = extract_tags(data);

    println!("file: {} ({})", file, file_index);
    println!("hunk: {}", hunk_index);
    println!("lines: {}", line_range.unwrap_or_else(|| "-".to_string()));
    if tags.is_empty() {
        println!("tags: (none)");
    } else {
        println!("tags: {}", tags.join(", "));
    }
}

fn print_text_tags_action(data: &Value, action: &str) {
    println!("tags {}:", action);
    print_text_tags_current(data);
}

fn print_text_full_context(data: &Value) {
    println!("== status ==");
    if let Some(status) = data.get("status") {
        print_text_status(status);
    }
    println!("\n== current_hunk ==");
    if let Some(current_hunk) = data.get("current_hunk") {
        if current_hunk.is_null() {
            println!("none");
        } else {
            print_text_current_hunk(current_hunk);
        }
    }
    println!("\n== annotations ==");
    if let Some(annotations) = data.get("annotations") {
        print_text_annotations(&serde_json::json!({ "annotations": annotations }));
    }
}

fn print_context_block(label: &str, value: Option<&Value>) {
    if let Some(lines) = value.and_then(|v| v.as_array()) {
        if !lines.is_empty() {
            println!("\n{}:", label);
            for line in lines {
                if let Some(text) = line.as_str() {
                    println!("{}", text);
                }
            }
        }
    }
}

fn extract_tags(data: &Value) -> Vec<String> {
    data.get("tags")
        .and_then(|v| v.as_array())
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

async fn fetch_current_tags(base_url: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let data = get(base_url, "/tags/current").await?;
    Ok(extract_tags(&data))
}

async fn fetch_hunk_tags(
    base_url: &str,
    file_index: usize,
    hunk_index: usize,
) -> Result<Vec<String>, Box<dyn Error>> {
    let payload = serde_json::json!({
        "file_index": file_index,
        "hunk_index": hunk_index,
    });
    let data = post(base_url, "/hunk", payload).await?;
    Ok(extract_tags(&data))
}

fn target_indices(
    file_index: Option<usize>,
    hunk_index: Option<usize>,
) -> Result<Option<(usize, usize)>, Box<dyn Error>> {
    match (file_index, hunk_index) {
        (Some(file_index), Some(hunk_index)) => Ok(Some((file_index, hunk_index))),
        (None, None) => Ok(None),
        _ => Err("Both --file-index and --hunk-index must be provided".into()),
    }
}

fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized: Vec<String> = tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn format_range(value: Option<&Value>) -> Option<String> {
    let arr = value.and_then(|v| v.as_array())?;
    if arr.len() != 2 {
        return None;
    }
    let start = arr.get(0)?.as_u64()?;
    let end = arr.get(1)?.as_u64()?;
    Some(format!("{}-{}", start, end))
}

fn filter_hunks_with_tags(data: &Value) -> Value {
    let mut hunks = data
        .get("hunks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    hunks.retain(|hunk| !extract_tags(hunk).is_empty());
    serde_json::json!({
        "scope": data.get("scope").cloned().unwrap_or(Value::Null),
        "hunks": hunks,
    })
}

fn format_annotation_line(annotation: &Value) -> Option<String> {
    let obj = annotation.as_object()?;
    let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("-");
    let file = obj.get("file").and_then(|v| v.as_str()).unwrap_or("-");
    let hunk = obj
        .get("hunk_index")
        .and_then(|v| v.as_u64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let range = format_range(obj.get("line_range")).unwrap_or_else(|| "-".to_string());
    let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
    Some(format!(
        "[{}] {}:{} (hunk {}) {}",
        id, file, range, hunk, content
    ))
}

fn format_scope(scope: Option<&Value>) -> String {
    let scope = match scope.and_then(|v| v.as_object()) {
        Some(scope) => scope,
        None => return "unknown".to_string(),
    };
    let scope_type = scope
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let vcs = scope
        .get("vcs")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let mut parts = vec![scope_type.to_string(), format!("vcs={}", vcs)];
    if scope_type == "working_tree" {
        if let Some(base) = scope.get("base_commit_id").and_then(|v| v.as_str()) {
            parts.push(format!("base_commit_id={}", base));
        }
    }
    if scope_type == "commit" {
        if let Some(commit) = scope.get("commit_id").and_then(|v| v.as_str()) {
            parts.push(format!("commit_id={}", commit));
        }
    }
    if let Some(diff_ref) = scope.get("diff_reference").and_then(|v| v.as_str()) {
        parts.push(format!("diff_reference={}", diff_ref));
    }
    parts.join(" ")
}
