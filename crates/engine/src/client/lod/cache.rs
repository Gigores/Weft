//! A bounded, disposable cache of finished tile meshes. Collisions replace a slot.
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Counts, LodMesh, MAX_QUADS, TileKey};
use rustc_hash::FxHashMap;

// Bump when the mesher or packed face format changes.
const VERSION: u32 = 10;
pub(super) const SLOTS: u64 = 4096;
const MAX_FILE: usize = 128 << 10;
const HEADER: usize = size_of::<Counts>() + 8;

pub(super) struct Entry {
    path: PathBuf,
    alternate: PathBuf,
    signature: u64,
}

impl Entry {
    pub(super) fn new(dir: &Path, dim: u32, seed: u64, key: TileKey, terrain: u64, columns: u64) -> Self {
        let mut hash = DefaultHasher::new();
        (dim, seed, key).hash(&mut hash);
        let address = hash.finish();
        let slot = address % SLOTS;
        let other = (slot + ((address >> 32) | 1)) % SLOTS;
        (VERSION, terrain, columns).hash(&mut hash);
        Self { path: dir.join(format!("{slot:03x}")), alternate: dir.join(format!("{other:03x}")), signature: hash.finish() }
    }

    pub(super) fn load(&mut self, key: TileKey) -> Option<LodMesh> {
        let mut first_occupied = false;
        let mut write_alternate = false;
        for (i, path) in [&self.path, &self.alternate].into_iter().enumerate() {
            let Ok(mut file) = File::open(path) else {
                write_alternate = i == 1 && first_occupied;
                continue;
            };
            first_occupied |= i == 0;
            let mut tag = [0; 8];
            if file.read_exact(&mut tag).is_err() || tag != self.signature.to_le_bytes() {
                continue;
            }
            let mut data = Vec::from(tag);
            file.take(MAX_FILE as u64 - 7).read_to_end(&mut data).ok()?;
            if let Some(mesh) = self.decode(key, &data) {
                return Some(mesh);
            }
            write_alternate = i == 1;
            break;
        }
        if write_alternate {
            std::mem::swap(&mut self.path, &mut self.alternate);
        }
        None
    }

    fn decode(&self, key: TileKey, data: &[u8]) -> Option<LodMesh> {
        if data.len() > MAX_FILE || data.get(..8)? != self.signature.to_le_bytes() {
            return None;
        }
        let compressed = data.get(16..)?;
        let size = u32::from_le_bytes(compressed.get(..4)?.try_into().ok()?) as usize;
        if !(HEADER..=HEADER + MAX_QUADS * 16).contains(&size)
            || (size - HEADER) % 16 != 0
            || data[8..16] != checksum(compressed).to_le_bytes()
        {
            return None;
        }
        let raw = lz4_flex::decompress_size_prepended(compressed).ok()?;
        if raw.len() != size {
            return None;
        }
        let y = (i32::from_le_bytes(raw[..4].try_into().ok()?), i32::from_le_bytes(raw[4..8].try_into().ok()?));
        let counts: Counts = bytemuck::pod_read_unaligned(&raw[8..HEADER]);
        let n = (size - HEADER) / 16;
        if counts.iter().flatten().map(|&n| n as u64).sum::<u64>() != n as u64 || y.1.checked_sub(y.0)? < 0 {
            return None;
        }
        let quads = raw[HEADER..]
            .chunks_exact(16)
            .map(bytemuck::pod_read_unaligned)
            .collect();
        Some(LodMesh { key, counts, y, quads })
    }

    pub(super) fn store(&self, mesh: &LodMesh) {
        if mesh.quads.len() > MAX_QUADS {
            return;
        }
        let mut raw = Vec::with_capacity(HEADER + mesh.quads.len() * 16);
        raw.extend_from_slice(&mesh.y.0.to_le_bytes());
        raw.extend_from_slice(&mesh.y.1.to_le_bytes());
        raw.extend_from_slice(bytemuck::bytes_of(&mesh.counts));
        raw.extend_from_slice(bytemuck::cast_slice(&mesh.quads));
        let data = lz4_flex::compress_prepend_size(&raw);
        if data.len() + 16 > MAX_FILE {
            return;
        }
        replace(&self.path, |file| {
            file.write_all(&self.signature.to_le_bytes())?;
            file.write_all(&checksum(&data).to_le_bytes())?;
            file.write_all(&data)
        });
    }
}

/// The small height index lets selection descend directly to cached detail on a new visit.
/// Geometry still goes through Entry::load with the current terrain and saved-column signatures.
pub(super) type Heights = FxHashMap<TileKey, (i32, i32)>;

fn view_file(dir: &Path, dim: u32, seed: u64, terrain: u64) -> (PathBuf, [u8; 8]) {
    let mut hash = DefaultHasher::new();
    (dim, seed).hash(&mut hash);
    // At most 64 world/dimension indices, about 5 MiB across all worlds.
    let path = dir.join(format!("view-{:02x}", hash.finish() % 64));
    (VERSION, terrain).hash(&mut hash);
    (path, hash.finish().to_le_bytes())
}

pub(super) fn read_heights(dir: &Path, dim: u32, seed: u64, terrain: u64) -> Heights {
    let read = || -> Option<Heights> {
        let mut data = Vec::new();
        let (path, signature) = view_file(dir, dim, seed, terrain);
        File::open(path)
            .ok()?
            .take(17 + SLOTS * 20)
            .read_to_end(&mut data)
            .ok()?;
        let rows = data.get(16..)?;
        if rows.len() % 20 != 0 || rows.len() / 20 > SLOTS as usize || data[..8] != signature || data[8..16] != checksum(rows).to_le_bytes()
        {
            return None;
        }
        let mut heights = Heights::default();
        for row in rows.chunks_exact(20) {
            let [level, x, z, lo, hi] = std::array::from_fn(|i| i32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap()));
            if (0..=super::MAX_LEVEL as i32).contains(&level) && lo <= hi {
                heights.insert(TileKey { level: level as u32, x, z }, (lo, hi));
            }
        }
        Some(heights)
    };
    read().unwrap_or_default()
}

pub(super) fn write_heights(dir: &Path, dim: u32, seed: u64, terrain: u64, tiles: impl Iterator<Item = (TileKey, (i32, i32))>) {
    let mut rows = Vec::new();
    for (key, (lo, hi)) in tiles.take(SLOTS as usize) {
        for value in [key.level as i32, key.x, key.z, lo, hi] {
            rows.extend_from_slice(&value.to_le_bytes());
        }
    }
    let (path, signature) = view_file(dir, dim, seed, terrain);
    replace(&path, |file| {
        file.write_all(&signature)?;
        file.write_all(&checksum(&rows).to_le_bytes())?;
        file.write_all(&rows)
    });
}

// A second client can use the same cache, or an edit can catch a job in flight.
fn replace(path: &Path, write: impl FnOnce(&mut File) -> std::io::Result<()>) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!("{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let open = || File::create(&tmp);
    let file = open().or_else(|e| {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e);
        }
        fs::create_dir_all(path.parent().unwrap())?;
        open()
    });
    let Ok(mut file) = file else { return };
    let written = write(&mut file);
    drop(file);
    if written.and_then(|_| fs::rename(&tmp, path)).is_err() {
        let _ = fs::remove_file(tmp);
    }
}

fn checksum(data: &[u8]) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(data);
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_round_trip_and_invalid_entries() {
        let dir = std::env::temp_dir().join(format!("weft-lod-cache-{}-{:?}", std::process::id(), std::thread::current().id()));
        fs::create_dir_all(&dir).unwrap();
        let key = TileKey { level: 4, x: -3, z: 8 };
        let mut entry = Entry::new(&dir, 3, 42, key, 17, 21);
        let mut counts = Counts::default();
        counts[0][3] = 1;
        counts[2][1] = 1;
        let mesh = LodMesh { key, counts, y: (-64, 120), quads: vec![[7, 6, 5, 4], [9, 10, 11, 12]] };
        entry.store(&mesh);
        let loaded = entry.load(key).unwrap();
        assert_eq!((loaded.counts, loaded.y, loaded.quads), (mesh.counts, mesh.y, mesh.quads));
        let bytes = fs::read(&entry.path).unwrap();
        for (terrain, edits) in [(18, 21), (17, 22)] {
            assert!(
                Entry::new(&dir, 3, 42, key, terrain, edits)
                    .decode(key, &bytes)
                    .is_none()
            );
        }
        for len in 0..bytes.len() {
            assert!(entry.decode(key, &bytes[..len]).is_none());
        }
        let mut corrupt = bytes;
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(entry.decode(key, &corrupt).is_none());
        let other = TileKey { level: 0, x: -19, z: 32 };
        let heights = Heights::from_iter([(key, mesh.y), (other, (-5, 17))]);
        write_heights(&dir, 3, 42, 17, heights.iter().map(|(&k, &y)| (k, y)));
        assert_eq!(read_heights(&dir, 3, 42, 17), heights);
        assert!(read_heights(&dir, 3, 42, 18).is_empty());
        assert!(read_heights(&dir, 3, 43, 17).is_empty());
        fs::remove_file(view_file(&dir, 3, 42, 17).0).unwrap();
        fs::remove_file(entry.path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
