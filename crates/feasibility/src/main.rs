use clap::{Parser, Subcommand};
use fasttalk_feasibility::{evaluate, load_config, load_evidence, run_preflight};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "fasttalk-feasibility")]
#[command(about = "FastTalk v2 hardware feasibility gate")]
struct Cli {
    #[arg(long, default_value = "config/feasibility.json", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Preflight {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Evaluate {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<bool, String> {
    let cli = Cli::parse();
    let config = load_config(&cli.config)?;

    match cli.command {
        Commands::Preflight { root, output } => {
            let report = run_preflight(&config, &root);
            write_json(&report, output.as_deref())?;
            Ok(report.pass)
        }
        Commands::Evaluate { input, output } => {
            let evidence = load_evidence(&input)?;
            let report = evaluate(&config, &evidence);
            write_json(&report, output.as_deref())?;
            Ok(report.pass)
        }
    }
}

fn write_json<T: Serialize>(value: &T, output: Option<&Path>) -> Result<(), String> {
    let mut json = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    json.push('\n');
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        fs::write(path, &json)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    print!("{json}");
    Ok(())
}
