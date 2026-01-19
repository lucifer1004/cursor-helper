//! cursor-helper: CLI for Cursor IDE operations not exposed in the UI
//!
//! This tool is not affiliated with or endorsed by Anysphere, Inc. (Cursor).
//! It accesses locally stored data on your machine for personal use.
//! See DISCLAIMER.md for details.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;
use std::path::PathBuf;

mod commands;
mod config;
mod cursor;

#[derive(Parser)]
#[command(name = "cursor-helper")]
#[command(about = "CLI helper for Cursor IDE operations", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Rename or copy a Cursor project while preserving history
    Rename {
        /// Old project path
        old_path: String,

        /// New project path
        new_path: String,

        /// Show what would be done without making changes
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Copy instead of move (keeps original project intact)
        #[arg(short, long)]
        copy: bool,
    },

    /// List all Cursor projects
    List {
        /// Show workspace hash for each project
        #[arg(long)]
        with_hash: bool,

        /// Sort by: name, modified, chats (default: modified)
        #[arg(long, short, default_value = "modified")]
        sort: String,

        /// Reverse sort order
        #[arg(long, short)]
        reverse: bool,

        /// Filter: local, remote, or pattern to match path
        #[arg(long, short)]
        filter: Option<String>,

        /// Limit number of results
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },

    /// Show usage statistics for a project
    Stats {
        /// Project path (prompts if omitted)
        project_path: Option<String>,
    },

    /// Export chat history to a readable format
    ExportChat {
        /// Project path
        project_path: String,

        /// Output format: md or json (default: md)
        #[arg(long, short, default_value = "md")]
        format: String,

        /// Output file (prints to stdout if omitted)
        #[arg(long, short)]
        output: Option<String>,

        /// Include thinking/reasoning blocks
        #[arg(long)]
        with_thinking: bool,

        /// Include tool calls (file reads, edits, commands)
        #[arg(long)]
        with_tools: bool,

        /// Include model info and token counts
        #[arg(long)]
        with_stats: bool,

        /// Include all extra data (thinking, tools, stats)
        #[arg(short, long)]
        verbose: bool,

        /// Include archived chat sessions
        #[arg(long)]
        include_archived: bool,
    },

    /// Remove orphaned workspace storage (projects that no longer exist)
    Clean {
        /// Show what would be deleted without making changes
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Backup Cursor metadata for a project
    Backup {
        /// Project path to backup
        project_path: String,

        /// Output backup file (will add .tar.gz if not present)
        backup_file: String,
    },

    /// Restore Cursor metadata from a backup
    Restore {
        /// Backup file to restore from
        backup_file: String,

        /// New project path to restore to
        new_path: String,
    },

    /// Clone a project with full chat history to a new location
    Clone {
        /// Source project path
        old_path: String,

        /// Destination project path
        new_path: String,

        /// Show what would be done without making changes
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Rename {
            old_path,
            new_path,
            dry_run,
            copy,
        } => {
            if dry_run {
                println!("{}", "(DRY-RUN MODE - no changes will be made)".blue());
            }
            commands::rename::execute(&old_path, &new_path, dry_run, copy)?;
        }

        Commands::List {
            with_hash,
            sort,
            reverse,
            filter,
            limit,
        } => {
            let workspace_storage_dir = config::workspace_storage_dir()
                .context("Failed to determine workspace storage directory")?;

            let mut projects = commands::list::list(workspace_storage_dir)?;

            // Apply filter
            if let Some(ref filter_str) = filter {
                projects.retain(|p| {
                    let path_str = p.path.to_string_lossy();
                    match filter_str.as_str() {
                        "local" => p.remote.is_none(),
                        "remote" => p.remote.is_some(),
                        pattern => path_str.contains(pattern),
                    }
                });
            }

            // Apply sorting
            match sort.as_str() {
                "name" => {
                    projects.sort_by(|a, b| a.path.cmp(&b.path));
                }
                "chats" => {
                    projects.sort_by(|a, b| b.chat_count.cmp(&a.chat_count));
                }
                _ => {
                    // Default (including "modified"): already sorted by modified in list()
                }
            }

            // Reverse if requested
            if reverse {
                projects.reverse();
            }

            // Apply limit
            let total_count = projects.len();
            if let Some(n) = limit {
                projects.truncate(n);
            }

            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic);

            // Build header
            let mut header = vec![];
            if with_hash {
                header.push(Cell::new("Hash"));
            }
            header.push(Cell::new("Remote"));
            header.push(Cell::new("Path"));
            header.push(Cell::new("Chats"));
            header.push(Cell::new("Modified"));
            table.set_header(header);

            for project in &projects {
                let path_str = project.path.to_string_lossy().to_string();
                let chat_str = project.chat_count.to_string();
                let remote_str = match &project.remote {
                    Some(r) => format!("{}:{}", r.remote_type, r.name),
                    None => "-".to_string(),
                };
                let modified_str = project
                    .last_modified
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| {
                        let secs = d.as_secs();
                        let dt =
                            chrono::DateTime::from_timestamp(secs as i64, 0).unwrap_or_default();
                        dt.format("%Y-%m-%d %H:%M").to_string()
                    })
                    .unwrap_or_else(|| "-".to_string());

                let mut row = vec![];
                if with_hash {
                    row.push(Cell::new(&project.folder_id));
                }
                row.push(Cell::new(remote_str));
                row.push(Cell::new(path_str));
                row.push(Cell::new(chat_str));
                row.push(Cell::new(modified_str));
                table.add_row(row);
            }

            println!("{table}");
            if projects.len() < total_count {
                println!("\nShowing {} of {} projects", projects.len(), total_count);
            } else {
                println!("\n{} projects found", total_count);
            }
        }

        Commands::Stats { project_path } => {
            let project_path = project_path.map(PathBuf::from);
            let stats = commands::stats::stats(project_path)?;
            println!("{}", commands::stats::format_stats(&stats));
        }

        Commands::ExportChat {
            project_path,
            format,
            output,
            with_thinking,
            with_tools,
            with_stats,
            verbose,
            include_archived,
        } => {
            let format = commands::export_chat::ExportFormat::from_str(&format)
                .context("Invalid format. Use 'md' or 'json'")?;
            let options = commands::export_chat::ExportOptions {
                with_thinking: with_thinking || verbose,
                with_tools: with_tools || verbose,
                with_stats: with_stats || verbose,
                include_archived,
            };
            commands::export_chat::execute(&project_path, format, output.as_deref(), &options)?;
        }

        Commands::Clean { dry_run, yes } => {
            if dry_run {
                println!("{}", "(DRY-RUN MODE - no changes will be made)".blue());
            }
            commands::clean::execute(dry_run, yes)?;
        }

        Commands::Backup {
            project_path,
            backup_file,
        } => {
            commands::backup::execute(&project_path, &backup_file)?;
        }

        Commands::Restore {
            backup_file,
            new_path,
        } => {
            commands::restore::execute(&backup_file, &new_path)?;
        }

        Commands::Clone {
            old_path,
            new_path,
            dry_run,
        } => {
            if dry_run {
                println!("{}", "(DRY-RUN MODE - no changes will be made)".blue());
            }
            commands::clone::execute(&old_path, &new_path, dry_run)?;
        }
    }

    Ok(())
}
