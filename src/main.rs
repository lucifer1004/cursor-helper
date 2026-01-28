//! cursor-helper: CLI for Cursor IDE operations not exposed in the UI
//!
//! This tool is not affiliated with or endorsed by Anysphere, Inc. (Cursor).
//! It accesses locally stored data on your machine for personal use.
//! See DISCLAIMER.md for details.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
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
        /// Show workspace ID for each project (use with export-chat --workspace-id)
        #[arg(long)]
        with_id: bool,

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
        /// Project path (local or remote, e.g., /home/user/project for SSH)
        project_path: Option<String>,

        /// Workspace ID (hash) - use instead of project_path for direct lookup
        #[arg(long, conflicts_with = "project_path")]
        workspace_id: Option<String>,

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

        /// Split output into separate files per session (requires --output as directory)
        #[arg(long)]
        split: bool,

        /// Exclude sessions with no messages
        #[arg(long)]
        exclude_blank: bool,
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
            with_id,
            sort,
            reverse,
            filter,
            limit,
        } => {
            let options = commands::list::ListOptions {
                with_id,
                sort,
                reverse,
                filter,
                limit,
            };
            let output = commands::list::execute(options)?;
            println!("{}", output);
        }

        Commands::Stats { project_path } => {
            let project_path = project_path.map(PathBuf::from);
            let stats = commands::stats::stats(project_path)?;
            println!("{}", commands::stats::format_stats(&stats));
        }

        Commands::ExportChat {
            project_path,
            workspace_id,
            format,
            output,
            with_thinking,
            with_tools,
            with_stats,
            verbose,
            include_archived,
            split,
            exclude_blank,
        } => {
            let format = commands::export_chat::ExportFormat::from_str(&format)
                .context("Invalid format. Use 'md' or 'json'")?;
            let options = commands::export_chat::ExportOptions {
                with_thinking: with_thinking || verbose,
                with_tools: with_tools || verbose,
                with_stats: with_stats || verbose,
                include_archived,
                exclude_blank,
            };

            // Either project_path or workspace_id must be provided
            match (project_path, workspace_id) {
                (Some(path), None) => {
                    commands::export_chat::execute(
                        &path,
                        format,
                        output.as_deref(),
                        &options,
                        split,
                    )?;
                }
                (None, Some(id)) => {
                    commands::export_chat::execute_by_id(
                        &id,
                        format,
                        output.as_deref(),
                        &options,
                        split,
                    )?;
                }
                (None, None) => {
                    anyhow::bail!("Either project_path or --workspace-id must be provided");
                }
                (Some(_), Some(_)) => {
                    // This case is prevented by clap's conflicts_with
                    unreachable!()
                }
            }
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
