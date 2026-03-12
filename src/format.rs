use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::graph::{DependencyKind, PackageGraph};

const MAGIC: [u8; 4] = *b"GCR1";
const NO_STRING: u32 = u32::MAX;

#[derive(Debug, Clone)]
pub struct StoredGraph {
    pub strings: Vec<String>,
    pub packages: Vec<StoredPackage>,
    pub dependencies: Vec<StoredDependency>,
}

#[derive(Debug, Clone)]
pub struct StoredPackage {
    pub crate_id: u32,
    pub name: u32,
    pub version: u32,
    pub downloads: u64,
    pub dependency_start: u32,
    pub dependency_count: u32,
}

#[derive(Debug, Clone)]
pub struct StoredDependency {
    pub package_index: u32,
    pub req: u32,
    pub target: u32,
    pub kind: u8,
    pub flags: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct GraphStats {
    pub package_count: usize,
    pub dependency_count: usize,
}

impl StoredGraph {
    pub fn from_package_graph(graph: &PackageGraph) -> Self {
        let mut pool = StringPool::default();
        let mut packages = Vec::with_capacity(graph.packages.len());
        let mut dependencies = Vec::with_capacity(graph.dependencies.len());

        for package in &graph.packages {
            let name = pool.intern(&package.name);
            let version = pool.intern(&package.version);
            packages.push(StoredPackage {
                crate_id: package.crate_id,
                name,
                version,
                downloads: package.downloads,
                dependency_start: package.dependency_start,
                dependency_count: package.dependency_count,
            });
        }

        for dependency in &graph.dependencies {
            let req = pool.intern(&dependency.req);
            let target = dependency
                .target
                .as_deref()
                .map(|target| pool.intern(target))
                .unwrap_or(NO_STRING);

            dependencies.push(StoredDependency {
                package_index: dependency.package_index,
                req,
                target,
                kind: dependency.kind.code(),
                flags: dependency.flags(),
            });
        }

        Self {
            strings: pool.finish(),
            packages,
            dependencies,
        }
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create graph output directory {}", parent.display())
            })?;
        }

        let file = File::create(path)
            .with_context(|| format!("failed to create graph file {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        self.write_to(&mut writer)?;
        writer
            .flush()
            .with_context(|| format!("failed to flush graph file {}", path.display()))?;
        Ok(())
    }

    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(&MAGIC).context("failed to write graph magic")?;
        write_u32(writer, self.strings.len() as u32)?;
        write_u32(writer, self.packages.len() as u32)?;
        write_u32(writer, self.dependencies.len() as u32)?;

        for string in &self.strings {
            write_bytes(writer, string.as_bytes())?;
        }

        for package in &self.packages {
            write_u32(writer, package.crate_id)?;
            write_u32(writer, package.name)?;
            write_u32(writer, package.version)?;
            write_u64(writer, package.downloads)?;
            write_u32(writer, package.dependency_start)?;
            write_u32(writer, package.dependency_count)?;
        }

        for dependency in &self.dependencies {
            write_u32(writer, dependency.package_index)?;
            write_u32(writer, dependency.req)?;
            write_u32(writer, dependency.target)?;
            write_u8(writer, dependency.kind)?;
            write_u8(writer, dependency.flags)?;
            write_u16(writer, 0)?;
        }

        Ok(())
    }

    pub fn read_from_path(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("failed to open graph file {}", path.display()))?;
        let mut reader = BufReader::new(file);
        Self::read_from(&mut reader)
    }

    pub fn read_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut magic = [0_u8; 4];
        reader
            .read_exact(&mut magic)
            .context("failed to read graph magic")?;
        if magic != MAGIC {
            bail!("invalid graph file magic");
        }

        let string_count = read_u32(reader)? as usize;
        let package_count = read_u32(reader)? as usize;
        let dependency_count = read_u32(reader)? as usize;

        let mut strings = Vec::with_capacity(string_count);
        for _ in 0..string_count {
            let bytes = read_bytes(reader)?;
            let string = String::from_utf8(bytes).context("graph file contains invalid utf-8")?;
            strings.push(string);
        }

        let mut packages = Vec::with_capacity(package_count);
        for _ in 0..package_count {
            packages.push(StoredPackage {
                crate_id: read_u32(reader)?,
                name: read_u32(reader)?,
                version: read_u32(reader)?,
                downloads: read_u64(reader)?,
                dependency_start: read_u32(reader)?,
                dependency_count: read_u32(reader)?,
            });
        }

        let mut dependencies = Vec::with_capacity(dependency_count);
        for _ in 0..dependency_count {
            let package_index = read_u32(reader)?;
            let req = read_u32(reader)?;
            let target = read_u32(reader)?;
            let kind = read_u8(reader)?;
            let flags = read_u8(reader)?;
            let _reserved = read_u16(reader)?;
            dependencies.push(StoredDependency {
                package_index,
                req,
                target,
                kind,
                flags,
            });
        }

        Ok(Self {
            strings,
            packages,
            dependencies,
        })
    }

    pub fn stats(&self) -> GraphStats {
        GraphStats {
            package_count: self.packages.len(),
            dependency_count: self.dependencies.len(),
        }
    }

    pub fn resolve(&self, index: u32) -> Option<&str> {
        self.strings.get(index as usize).map(String::as_str)
    }

    pub fn package_by_name(&self, name: &str) -> Option<(usize, &StoredPackage)> {
        let query = name.trim();
        if query.is_empty() {
            return None;
        }
        let query_lower = query.to_ascii_lowercase();

        self.packages
            .iter()
            .enumerate()
            .filter(|(_, package)| {
                self.resolve(package.name)
                    .map(|candidate| candidate.eq_ignore_ascii_case(&query_lower))
                    .unwrap_or(false)
            })
            .max_by(|left, right| {
                left.1
                    .downloads
                    .cmp(&right.1.downloads)
                    .then(left.1.dependency_count.cmp(&right.1.dependency_count))
                    .then(right.1.crate_id.cmp(&left.1.crate_id))
            })
    }

    pub fn dependency_slice(&self, package: &StoredPackage) -> &[StoredDependency] {
        let start = package.dependency_start as usize;
        let end = start + package.dependency_count as usize;
        &self.dependencies[start..end]
    }
}

impl StoredDependency {
    pub fn kind(&self) -> DependencyKind {
        DependencyKind::from_code(self.kind)
    }

    pub fn optional(&self) -> bool {
        self.flags & 0b0000_0001 != 0
    }

    pub fn uses_default_features(&self) -> bool {
        self.flags & 0b0000_0010 != 0
    }
}

#[derive(Default)]
struct StringPool {
    strings: Vec<String>,
    index_by_value: HashMap<String, u32>,
}

impl StringPool {
    fn intern(&mut self, value: &str) -> u32 {
        if let Some(index) = self.index_by_value.get(value) {
            return *index;
        }

        let index = self.strings.len() as u32;
        let owned = value.to_owned();
        self.strings.push(owned.clone());
        self.index_by_value.insert(owned, index);
        index
    }

    fn finish(self) -> Vec<String> {
        self.strings
    }
}

fn write_bytes<W: Write>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    write_u32(writer, bytes.len() as u32)?;
    writer
        .write_all(bytes)
        .context("failed to write graph string bytes")?;
    Ok(())
}

fn write_u8<W: Write>(writer: &mut W, value: u8) -> Result<()> {
    writer
        .write_all(&[value])
        .context("failed to write u8 to graph file")?;
    Ok(())
}

fn write_u16<W: Write>(writer: &mut W, value: u16) -> Result<()> {
    writer
        .write_all(&value.to_le_bytes())
        .context("failed to write u16 to graph file")?;
    Ok(())
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> Result<()> {
    writer
        .write_all(&value.to_le_bytes())
        .context("failed to write u32 to graph file")?;
    Ok(())
}

fn write_u64<W: Write>(writer: &mut W, value: u64) -> Result<()> {
    writer
        .write_all(&value.to_le_bytes())
        .context("failed to write u64 to graph file")?;
    Ok(())
}

fn read_bytes<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let len = read_u32(reader)? as usize;
    let mut bytes = vec![0_u8; len];
    reader
        .read_exact(&mut bytes)
        .context("failed to read graph string bytes")?;
    Ok(bytes)
}

fn read_u8<R: Read>(reader: &mut R) -> Result<u8> {
    let mut bytes = [0_u8; 1];
    reader
        .read_exact(&mut bytes)
        .context("failed to read u8 from graph file")?;
    Ok(bytes[0])
}

fn read_u16<R: Read>(reader: &mut R) -> Result<u16> {
    let mut bytes = [0_u8; 2];
    reader
        .read_exact(&mut bytes)
        .context("failed to read u16 from graph file")?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0_u8; 4];
    reader
        .read_exact(&mut bytes)
        .context("failed to read u32 from graph file")?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64> {
    let mut bytes = [0_u8; 8];
    reader
        .read_exact(&mut bytes)
        .context("failed to read u64 from graph file")?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{StoredDependency, StoredGraph, StoredPackage};

    #[test]
    fn round_trip_graph_binary() {
        let graph = StoredGraph {
            strings: vec![
                "tokio".to_owned(),
                "1.0.0".to_owned(),
                "^1".to_owned(),
                "cfg(unix)".to_owned(),
            ],
            packages: vec![StoredPackage {
                crate_id: 1,
                name: 0,
                version: 1,
                downloads: 42,
                dependency_start: 0,
                dependency_count: 1,
            }],
            dependencies: vec![StoredDependency {
                package_index: 0,
                req: 2,
                target: 3,
                kind: 0,
                flags: 0b11,
            }],
        };

        let mut bytes = Vec::new();
        graph.write_to(&mut bytes).unwrap();

        let restored = StoredGraph::read_from(&mut bytes.as_slice()).unwrap();
        assert_eq!(restored.strings, graph.strings);
        assert_eq!(restored.packages.len(), graph.packages.len());
        assert_eq!(restored.dependencies.len(), graph.dependencies.len());
        assert_eq!(restored.dependencies[0].flags, graph.dependencies[0].flags);
    }
}
