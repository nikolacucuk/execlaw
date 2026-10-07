use clap::{Parser, Subcommand};
use execlaw_plugin_host::conformance::{check_project, check_upgrade, generate_project};
use execlaw_plugin_sdk::PluginManifest;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "execlaw-plugin-conformance", about = "Offline plugin author conformance kit")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a minimal script- or subprocess-tier project.
    Init {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        plugin_id: String,
        #[arg(long, value_parser = ["script", "subprocess"])]
        tier: String,
    },
    /// Run offline manifest, schema, mock-host, and lifecycle checks.
    Check { path: PathBuf },
    /// Check whether a candidate bundle safely upgrades an older manifest.
    Upgrade {
        #[arg(long)]
        previous: PathBuf,
        #[arg(long)]
        candidate: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Init { path, plugin_id, tier } => {
            generate_project(&path, &plugin_id, &tier)?;
            println!("generated {tier} plugin project at {}", path.display());
        }
        Command::Check { path } => {
            let report = check_project(&path)?;
            println!(
                "plugin={} runtime={} tools={} fixtures={} capabilities={}",
                report.plugin_id,
                report.runtime_tier,
                report.tool_count,
                report.case_count,
                if report.declared_capabilities.is_empty() {
                    "none".into()
                } else {
                    report.declared_capabilities.join(",")
                }
            );
        }
        Command::Upgrade { previous, candidate } => {
            let previous = read_manifest(&previous)?;
            let candidate = read_manifest(&candidate)?;
            check_upgrade(&previous, &candidate)?;
            println!("upgrade contract passed for {}", candidate.plugin.id);
        }
    }
    Ok(())
}

fn read_manifest(path: &std::path::Path) -> anyhow::Result<PluginManifest> {
    let source = std::fs::read_to_string(path)?;
    Ok(PluginManifest::parse(&source)?)
}
