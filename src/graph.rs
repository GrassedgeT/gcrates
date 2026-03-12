use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use csv::{Reader, StringRecord};
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;
use semver::Version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageGraph {
    pub packages: Vec<PackageEntry>,
    pub dependencies: Vec<DependencyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub crate_id: u32,
    pub name: String,
    pub version: String,
    pub downloads: u64,
    pub dependency_start: u32,
    pub dependency_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyEntry {
    pub package_index: u32,
    pub req: String,
    pub kind: DependencyKind,
    pub optional: bool,
    pub uses_default_features: bool,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct GraphBuildOptions {
    pub include_normal_dependencies: bool,
    pub include_build_dependencies: bool,
    pub include_dev_dependencies: bool,
    pub include_target_specific_dependencies: bool,
}

impl Default for GraphBuildOptions {
    fn default() -> Self {
        Self {
            include_normal_dependencies: true,
            include_build_dependencies: true,
            include_dev_dependencies: true,
            include_target_specific_dependencies: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Normal,
    Build,
    Dev,
    Unknown(u8),
}

impl DependencyKind {
    pub fn from_dump_value(value: &str) -> Result<Self> {
        let code = parse_u8(value)?;
        Ok(Self::from_code(code))
    }

    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Normal,
            1 => Self::Build,
            2 => Self::Dev,
            other => Self::Unknown(other),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Build => 1,
            Self::Dev => 2,
            Self::Unknown(code) => code,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
            Self::Unknown(_) => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BuildReport {
    pub package_count: usize,
    pub dependency_count: usize,
    pub skipped_yanked_versions: usize,
    pub skipped_non_semver_versions: usize,
}

pub fn build_graph_from_dump(dump_root: &Path, options: GraphBuildOptions) -> Result<(PackageGraph, BuildReport)> {
    let data_root = data_root(dump_root);
    let crates_path = data_root.join("crates.csv");
    let versions_path = data_root.join("versions.csv");
    let dependencies_path = data_root.join("dependencies.csv");

    let crate_names = load_crate_names(&crates_path)?;
    let (selected_versions, skipped_yanked_versions, skipped_non_semver_versions) =
        select_versions(&versions_path, &crate_names)?;
    let graph = build_petgraph(&dependencies_path, &selected_versions, options)?;
    let compact = compact_graph(&graph);

    let report = BuildReport {
        package_count: compact.packages.len(),
        dependency_count: compact.dependencies.len(),
        skipped_yanked_versions,
        skipped_non_semver_versions,
    };

    Ok((compact, report))
}

impl DependencyEntry {
    pub fn flags(&self) -> u8 {
        let mut flags = 0_u8;
        if self.optional {
            flags |= 0b0000_0001;
        }
        if self.uses_default_features {
            flags |= 0b0000_0010;
        }
        flags
    }
}

fn data_root(dump_root: &Path) -> PathBuf {
    let candidate = dump_root.join("data");
    if candidate.is_dir() {
        return candidate;
    }
    if dump_root.join("crates.csv").is_file() {
        return dump_root.to_path_buf();
    }
    dump_root.to_path_buf()
}

fn load_crate_names(path: &Path) -> Result<HashMap<u32, String>> {
    let mut reader = csv_reader(path)?;
    let headers = reader
        .headers()
        .with_context(|| format!("failed to read CSV header from {}", path.display()))?
        .clone();
    let id_index = required_column(&headers, "id")?;
    let name_index = required_column(&headers, "name")?;

    let mut names = HashMap::new();
    for record in reader.records() {
        let record =
            record.with_context(|| format!("failed to parse record in {}", path.display()))?;
        let crate_id = parse_u32(field(&record, id_index, "id")?)?;
        let name = field(&record, name_index, "name")?.to_owned();
        names.insert(crate_id, name);
    }

    Ok(names)
}

fn select_versions(
    path: &Path,
    crate_names: &HashMap<u32, String>,
) -> Result<(Vec<SelectedVersion>, usize, usize)> {
    let mut reader = csv_reader(path)?;
    let headers = reader
        .headers()
        .with_context(|| format!("failed to read CSV header from {}", path.display()))?
        .clone();

    let id_index = required_column(&headers, "id")?;
    let crate_id_index = required_column(&headers, "crate_id")?;
    let num_index = required_column(&headers, "num")?;
    let num_no_build_index = required_column(&headers, "num_no_build")?;
    let downloads_index = required_column(&headers, "downloads")?;
    let created_at_index = required_column(&headers, "created_at")?;
    let yanked_index = required_column(&headers, "yanked")?;

    let mut selected = HashMap::<u32, VersionCandidate>::new();
    let mut skipped_yanked_versions = 0_usize;
    let mut skipped_non_semver_versions = 0_usize;

    for record in reader.records() {
        let record =
            record.with_context(|| format!("failed to parse record in {}", path.display()))?;

        let crate_id = parse_u32(field(&record, crate_id_index, "crate_id")?)?;
        if !crate_names.contains_key(&crate_id) {
            continue;
        }

        let yanked = parse_dump_bool(field(&record, yanked_index, "yanked")?)?;
        if yanked {
            skipped_yanked_versions += 1;
            continue;
        }

        let version_string = field(&record, num_index, "num")?.to_owned();
        let normalized_version = field(&record, num_no_build_index, "num_no_build")?;
        let parsed_version = Version::parse(normalized_version).ok();
        if parsed_version.is_none() {
            skipped_non_semver_versions += 1;
        }

        let candidate = VersionCandidate {
            crate_id,
            version_id: parse_u32(field(&record, id_index, "id")?)?,
            version: version_string,
            parsed_version,
            created_at: field(&record, created_at_index, "created_at")?.to_owned(),
            downloads: parse_u64(field(&record, downloads_index, "downloads")?)?,
        };

        match selected.get_mut(&crate_id) {
            Some(current) if candidate.cmp(current) == Ordering::Greater => {
                *current = candidate;
            }
            None => {
                selected.insert(crate_id, candidate);
            }
            _ => {}
        }
    }

    let mut versions = Vec::with_capacity(selected.len());
    for candidate in selected.into_values() {
        let Some(name) = crate_names.get(&candidate.crate_id) else {
            continue;
        };
        versions.push(SelectedVersion {
            crate_id: candidate.crate_id,
            version_id: candidate.version_id,
            name: name.clone(),
            version: candidate.version,
            downloads: candidate.downloads,
        });
    }

    versions.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.crate_id.cmp(&right.crate_id))
    });

    Ok((versions, skipped_yanked_versions, skipped_non_semver_versions))
}

fn build_petgraph(
    dependencies_path: &Path,
    selected_versions: &[SelectedVersion],
    options: GraphBuildOptions,
) -> Result<DiGraph<SelectedVersion, EdgeMetadata>> {
    let mut graph = DiGraph::<SelectedVersion, EdgeMetadata>::new();
    let mut crate_id_to_node = HashMap::with_capacity(selected_versions.len());
    let mut version_id_to_node = HashMap::with_capacity(selected_versions.len());

    for version in selected_versions {
        let node = graph.add_node(version.clone());
        crate_id_to_node.insert(version.crate_id, node);
        version_id_to_node.insert(version.version_id, node);
    }

    let mut reader = csv_reader(dependencies_path)?;
    let headers = reader
        .headers()
        .with_context(|| format!("failed to read CSV header from {}", dependencies_path.display()))?
        .clone();

    let version_id_index = required_column(&headers, "version_id")?;
    let crate_id_index = required_column(&headers, "crate_id")?;
    let req_index = required_column(&headers, "req")?;
    let kind_index = required_column(&headers, "kind")?;
    let optional_index = required_column(&headers, "optional")?;
    let default_features_index = required_column(&headers, "default_features")?;
    let target_index = required_column(&headers, "target")?;

    for record in reader.records() {
        let record = record.with_context(|| {
            format!("failed to parse dependency record in {}", dependencies_path.display())
        })?;

        let source_version_id = parse_u32(field(&record, version_id_index, "version_id")?)?;
        let Some(&source_node) = version_id_to_node.get(&source_version_id) else {
            continue;
        };

        let dependency_crate_id = parse_u32(field(&record, crate_id_index, "crate_id")?)?;
        let Some(&target_node) = crate_id_to_node.get(&dependency_crate_id) else {
            continue;
        };

        let kind = DependencyKind::from_dump_value(field(&record, kind_index, "kind")?)?;
        if !should_include_kind(kind, options) {
            continue;
        }

        let target = optional_string(field(&record, target_index, "target")?);
        if target.is_some() && !options.include_target_specific_dependencies {
            continue;
        }

        graph.add_edge(
            source_node,
            target_node,
            EdgeMetadata {
                req: field(&record, req_index, "req")?.to_owned(),
                kind,
                optional: parse_dump_bool(field(&record, optional_index, "optional")?)?,
                uses_default_features: parse_dump_bool(field(
                    &record,
                    default_features_index,
                    "default_features",
                )?)?,
                target,
            },
        );
    }

    Ok(graph)
}

fn compact_graph(graph: &DiGraph<SelectedVersion, EdgeMetadata>) -> PackageGraph {
    let mut package_indices: Vec<_> = graph.node_indices().collect();
    package_indices.sort_by_key(|index| index.index());

    let mut packages = Vec::with_capacity(package_indices.len());
    let mut dependencies = Vec::new();

    for node_index in package_indices {
        let package = &graph[node_index];
        let dependency_start = dependencies.len() as u32;

        let mut edges: Vec<_> = graph.edges(node_index).collect();
        edges.sort_by(|left, right| {
            let left_name = &graph[left.target()].name;
            let right_name = &graph[right.target()].name;
            left_name.cmp(right_name)
        });

        for edge in edges {
            dependencies.push(DependencyEntry {
                package_index: edge.target().index() as u32,
                req: edge.weight().req.clone(),
                kind: edge.weight().kind,
                optional: edge.weight().optional,
                uses_default_features: edge.weight().uses_default_features,
                target: edge.weight().target.clone(),
            });
        }

        packages.push(PackageEntry {
            crate_id: package.crate_id,
            name: package.name.clone(),
            version: package.version.clone(),
            downloads: package.downloads,
            dependency_start,
            dependency_count: dependencies.len() as u32 - dependency_start,
        });
    }

    PackageGraph {
        packages,
        dependencies,
    }
}

fn should_include_kind(kind: DependencyKind, options: GraphBuildOptions) -> bool {
    match kind {
        DependencyKind::Normal => options.include_normal_dependencies,
        DependencyKind::Build => options.include_build_dependencies,
        DependencyKind::Dev => options.include_dev_dependencies,
        DependencyKind::Unknown(_) => true,
    }
}

fn csv_reader(path: &Path) -> Result<Reader<std::fs::File>> {
    Reader::from_path(path).with_context(|| format!("failed to open {}", path.display()))
}

fn required_column(headers: &StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|header| header == name)
        .with_context(|| format!("missing required CSV column `{name}`"))
}

fn field<'record>(record: &'record StringRecord, index: usize, name: &str) -> Result<&'record str> {
    record
        .get(index)
        .with_context(|| format!("missing field `{name}` in CSV record"))
}

fn optional_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn parse_dump_bool(value: &str) -> Result<bool> {
    match value {
        "t" => Ok(true),
        "f" => Ok(false),
        other => bail!("invalid dump boolean value `{other}`"),
    }
}

fn parse_u8(value: &str) -> Result<u8> {
    value
        .parse::<u8>()
        .with_context(|| format!("failed to parse `{value}` as u8"))
}

fn parse_u32(value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .with_context(|| format!("failed to parse `{value}` as u32"))
}

fn parse_u64(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("failed to parse `{value}` as u64"))
}

#[derive(Debug, Clone)]
struct SelectedVersion {
    crate_id: u32,
    version_id: u32,
    name: String,
    version: String,
    downloads: u64,
}

#[derive(Debug, Clone)]
struct VersionCandidate {
    crate_id: u32,
    version_id: u32,
    version: String,
    parsed_version: Option<Version>,
    created_at: String,
    downloads: u64,
}

impl Ord for VersionCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        version_priority(self)
            .cmp(&version_priority(other))
            .then_with(|| cmp_semver(&self.parsed_version, &other.parsed_version))
            .then_with(|| self.created_at.cmp(&other.created_at))
            .then_with(|| self.downloads.cmp(&other.downloads))
            .then_with(|| self.version_id.cmp(&other.version_id))
    }
}

impl PartialOrd for VersionCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for VersionCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.version_id == other.version_id
    }
}

impl Eq for VersionCandidate {}

fn version_priority(candidate: &VersionCandidate) -> u8 {
    match &candidate.parsed_version {
        Some(version) if version.pre.is_empty() => 2,
        Some(_) => 1,
        None => 0,
    }
}

fn cmp_semver(left: &Option<Version>, right: &Option<Version>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(right),
        _ => Ordering::Equal,
    }
}

#[derive(Debug, Clone)]
struct EdgeMetadata {
    req: String,
    kind: DependencyKind,
    optional: bool,
    uses_default_features: bool,
    target: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{DependencyKind, VersionCandidate};
    use semver::Version;

    #[test]
    fn dependency_kind_matches_dump_codes() {
        assert_eq!(DependencyKind::from_code(0), DependencyKind::Normal);
        assert_eq!(DependencyKind::from_code(1), DependencyKind::Build);
        assert_eq!(DependencyKind::from_code(2), DependencyKind::Dev);
    }

    #[test]
    fn stable_semver_beats_prerelease() {
        let stable = VersionCandidate {
            crate_id: 1,
            version_id: 2,
            version: "1.0.0".to_owned(),
            parsed_version: Some(Version::parse("1.0.0").unwrap()),
            created_at: "2025-01-01 00:00:00+00".to_owned(),
            downloads: 10,
        };
        let prerelease = VersionCandidate {
            crate_id: 1,
            version_id: 3,
            version: "1.0.0-alpha.1".to_owned(),
            parsed_version: Some(Version::parse("1.0.0-alpha.1").unwrap()),
            created_at: "2025-01-02 00:00:00+00".to_owned(),
            downloads: 20,
        };

        assert!(stable > prerelease);
    }
}
