use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use konnect::{CONFIG_SCHEMA, Config, EXAMPLE_CONFIG, browser_host, config_path, kube_contexts};

#[cfg(target_os = "macos")]
mod tray;

#[cfg(not(target_os = "macos"))]
mod tray {
    use anyhow::{Result, bail};
    use konnect::Config;

    pub fn run(_: Config) -> Result<()> {
        bail!(
            "the Konnect toolbar is currently supported only on macOS; use `konnect list` on this platform"
        )
    }
}

#[derive(Debug, Parser)]
#[command(version, about = "Named local routes for Kubernetes port forwards")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write an example configuration file.
    Init {
        /// Replace an existing configuration file.
        #[arg(long)]
        force: bool,
    },
    /// Display configured routes without starting them.
    List,
    /// Start the toolbar application. This is the default command.
    Start,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = config_path()?;
    match cli.command {
        Some(Command::Init { force }) => init(path, force),
        Some(Command::List) => list(&path),
        None | Some(Command::Start) => {
            let config = Config::load(&path)?;
            tray::run(config)
        }
    }
}

fn init(path: PathBuf, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; use --force to replace it",
            path.display()
        );
    }
    let parent = path
        .parent()
        .context("configuration path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    fs::write(&path, EXAMPLE_CONFIG)
        .with_context(|| format!("failed to write {}", path.display()))?;
    let schema_path = parent.join("konnect.config.schema.json");
    fs::write(&schema_path, CONFIG_SCHEMA)
        .with_context(|| format!("failed to write {}", schema_path.display()))?;
    println!("Created {}", path.display());
    Ok(())
}

fn list(path: &std::path::Path) -> Result<()> {
    let config = Config::load(path)?;
    let contexts = kube_contexts()?;
    for forward in config.forwards_for_contexts(&contexts)? {
        println!(
            "{:<36} http://{}:{}  {} ({})",
            forward.route,
            browser_host(&forward.route),
            config.proxy.port,
            forward.target,
            forward.context,
        );
    }
    Ok(())
}
