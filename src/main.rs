use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use gcrates::download::{DEFAULT_DUMP_URL, download_and_extract};
use gcrates::format::StoredGraph;
use gcrates::graph::{GraphBuildOptions, build_graph_from_dump};

#[derive(Debug, Parser)]
#[command(author, version, about = "crates.io dependency graph tooling")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Download {
        #[arg(long, default_value = DEFAULT_DUMP_URL)]
        url: String,
        #[arg(long, default_value = "db-dump")]
        output: PathBuf,
    },
    BuildGraph {
        #[arg(long, default_value = "db-dump")]
        input: PathBuf,
        #[arg(long, default_value = "artifacts/graph.gcr")]
        output: PathBuf,
        #[arg(long)]
        exclude_normal: bool,
        #[arg(long)]
        exclude_build: bool,
        #[arg(long)]
        exclude_dev: bool,
        #[arg(long)]
        exclude_target_specific: bool,
    },
    Inspect {
        #[arg(long, default_value = "artifacts/graph.gcr")]
        graph: PathBuf,
        #[arg(long = "crate")]
        crate_name: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Download { url, output } => {
            println!("Downloading crates.io db dump from {url}");
            download_and_extract(&url, &output)?;
            println!("Extracted db dump into {}", output.display());
        }
        Commands::BuildGraph {
            input,
            output,
            exclude_normal,
            exclude_build,
            exclude_dev,
            exclude_target_specific,
        } => {
            let options = GraphBuildOptions {
                include_normal_dependencies: !exclude_normal,
                include_build_dependencies: !exclude_build,
                include_dev_dependencies: !exclude_dev,
                include_target_specific_dependencies: !exclude_target_specific,
            };
            let (graph, report) = build_graph_from_dump(&input, options)?;
            let stored = StoredGraph::from_package_graph(&graph);
            stored.write_to_path(&output)?;
            println!(
                "Wrote {} packages and {} dependencies to {}",
                report.package_count,
                report.dependency_count,
                output.display()
            );
            println!(
                "Skipped {} yanked versions and {} non-semver versions during selection",
                report.skipped_yanked_versions,
                report.skipped_non_semver_versions
            );
        }
        Commands::Inspect { graph, crate_name } => inspect_graph(&graph, crate_name.as_deref())?,
    }

    Ok(())
}

fn inspect_graph(path: &PathBuf, crate_name: Option<&str>) -> Result<()> {
    let graph = StoredGraph::read_from_path(path)
        .with_context(|| format!("failed to load graph file {}", path.display()))?;

    if let Some(crate_name) = crate_name {
        let Some((package_index, package)) = graph.package_by_name(crate_name) else {
            anyhow::bail!("crate `{crate_name}` not found in {}", path.display());
        };
        let version = graph.resolve(package.version).unwrap_or("<missing>");
        println!(
            "{} v{} (crate_id={}, downloads={})",
            crate_name, version, package.crate_id, package.downloads
        );
        println!("Direct dependencies:");
        for dependency in graph.dependency_slice(package) {
            let Some(target_package) = graph.packages.get(dependency.package_index as usize) else {
                continue;
            };
            let dependency_name = graph.resolve(target_package.name).unwrap_or("<missing>");
            let requirement = graph.resolve(dependency.req).unwrap_or("*");
            let target = match dependency.target {
                u32::MAX => String::new(),
                index => format!(" target={}", graph.resolve(index).unwrap_or("<missing>")),
            };
            let optional = if dependency.optional() {
                " optional"
            } else {
                ""
            };
            let default_features = if dependency.uses_default_features() {
                " default-features"
            } else {
                ""
            };
            println!(
                "- {} {} ({}){}{}{}",
                dependency_name,
                requirement,
                dependency.kind().as_str(),
                optional,
                default_features,
                target
            );
        }
        println!("Package index: {}", package_index);
    } else {
        let stats = graph.stats();
        println!("Packages: {}", stats.package_count);
        println!("Dependencies: {}", stats.dependency_count);
    }

    Ok(())
}
