use std::fs::{self, File, Metadata};
use std::hash::BuildHasher;
use std::io::{self, Read};
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use glam::IVec3;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};

use super::{ENTRIES, REGION, Region};

// The index can be discarded. Region headers remain the source of truth.
const NAME: &str = "regions.idx";
const MAGIC: &[u8] = b"WEFTRI1\0";
const STAMP_BYTES: usize = 20;
const MASK_BYTES: usize = ENTRIES / 8;
const RECORD_BYTES: usize = 12 + STAMP_BYTES + MASK_BYTES;

type Stamp = [u8; STAMP_BYTES];
type Mask = [u8; MASK_BYTES];

#[derive(Clone, Copy)]
struct Record {
    stamp: Stamp,
    occupied: Mask,
}

#[derive(Default)]
pub(super) struct RegionIndex {
    records: FxHashMap<IVec3, Record>,
    changed: FxHashSet<IVec3>,
    dirty: bool,
}

impl RegionIndex {
    pub(super) fn open(dir: &Path, mut region_found: impl FnMut(IVec3), mut cube_found: impl FnMut(IVec3)) -> Self {
        let start = Instant::now();
        let mut cached = fs::read(dir.join(NAME))
            .ok()
            .and_then(|bytes| decode(&bytes))
            .unwrap_or_default();
        let mut index = Self::default();
        let mut hits = 0;
        let mut scans = 0;
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let Some(pos) = region_pos(&entry.file_name().to_string_lossy()) else { continue };
            region_found(pos);
            let stamp = entry.metadata().ok().and_then(|meta| stamp(&meta));
            let old = cached.remove(&pos);
            let occupied = if let Some(record) = old.filter(|record| Some(record.stamp) == stamp) {
                hits += 1;
                record.occupied
            } else {
                scans += 1;
                index.dirty = true;
                let Ok(occupied) = read_mask(&entry.path()) else { continue };
                occupied
            };
            for (byte, bits) in occupied.iter().copied().enumerate() {
                let mut bits = bits;
                while bits != 0 {
                    let i = (byte * 8 + bits.trailing_zeros() as usize) as i32;
                    cube_found(pos * REGION + IVec3::new(i % REGION, i / (REGION * REGION), i / REGION % REGION));
                    bits &= bits - 1;
                }
            }
            if let Some(stamp) = stamp {
                index.records.insert(pos, Record { stamp, occupied });
            }
        }
        index.dirty |= !cached.is_empty();
        if hits + scans > 0 {
            log::debug!(target: "load", "region index {}: {hits} cached, {scans} headers read in {:.1} ms",
                dir.file_name().unwrap_or_default().to_string_lossy(), start.elapsed().as_secs_f64() * 1000.0);
        }
        index
    }

    pub(super) fn changed(&mut self, pos: IVec3) {
        self.changed.insert(pos);
        self.dirty = true;
    }

    pub(super) fn flush<'a>(&mut self, dir: &Path, mut region: impl FnMut(IVec3) -> Option<&'a Region>) {
        if !self.dirty {
            return;
        }
        for pos in self.changed.drain() {
            let record = if let Some(region) = region(pos) {
                region
                    .file
                    .metadata()
                    .ok()
                    .and_then(|meta| stamp(&meta))
                    .map(|stamp| Record { stamp, occupied: mask(region.header.iter().map(|&(_, len)| len != 0)) })
            } else {
                let path = dir.join(format!("r.{}.{}.{}.bin", pos.x, pos.y, pos.z));
                fs::metadata(&path)
                    .ok()
                    .and_then(|meta| stamp(&meta))
                    .and_then(|stamp| {
                        read_mask(&path)
                            .ok()
                            .map(|occupied| Record { stamp, occupied })
                    })
            };
            match record {
                Some(record) => {
                    self.records.insert(pos, record);
                }
                None => {
                    self.records.remove(&pos);
                }
            }
        }
        let path = dir.join(NAME);
        let tmp = path.with_extension("tmp");
        match fs::write(&tmp, encode(&self.records)).and_then(|()| fs::rename(&tmp, &path)) {
            Ok(()) => self.dirty = false,
            Err(e) => log::warn!("saving {}: {e}", path.display()),
        }
    }
}

fn region_pos(name: &str) -> Option<IVec3> {
    let mut parts = name.strip_prefix("r.")?.strip_suffix(".bin")?.split('.');
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let z = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some(IVec3::new(x, y, z))
}

fn stamp(meta: &Metadata) -> Option<Stamp> {
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let mut out = [0; STAMP_BYTES];
    out[..8].copy_from_slice(&meta.len().to_le_bytes());
    out[8..16].copy_from_slice(&modified.as_secs().to_le_bytes());
    out[16..].copy_from_slice(&modified.subsec_nanos().to_le_bytes());
    Some(out)
}

fn read_mask(path: &Path) -> io::Result<Mask> {
    let mut header = [0; ENTRIES * 8];
    File::open(path)?.read_exact(&mut header)?;
    Ok(mask(header.chunks_exact(8).map(|entry| entry[4..] != [0; 4])))
}

fn mask(occupied: impl Iterator<Item = bool>) -> Mask {
    let mut out = [0; MASK_BYTES];
    for (i, occupied) in occupied.enumerate() {
        if occupied {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

fn decode(bytes: &[u8]) -> Option<FxHashMap<IVec3, Record>> {
    let (checksum, records) = bytes.strip_prefix(MAGIC)?.split_first_chunk::<8>()?;
    if records.len() % RECORD_BYTES != 0 || FxBuildHasher.hash_one(records) != u64::from_le_bytes(*checksum) {
        return None;
    }
    let mut out = FxHashMap::default();
    for row in records.chunks_exact(RECORD_BYTES) {
        let point = std::array::from_fn(|i| i32::from_le_bytes(row[i * 4..i * 4 + 4].try_into().unwrap()));
        let stamp = row[12..12 + STAMP_BYTES].try_into().unwrap();
        let occupied = row[12 + STAMP_BYTES..].try_into().unwrap();
        out.insert(IVec3::from_array(point), Record { stamp, occupied });
    }
    Some(out)
}

fn encode(records: &FxHashMap<IVec3, Record>) -> Vec<u8> {
    let mut out = Vec::with_capacity(MAGIC.len() + 8 + records.len() * RECORD_BYTES);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[0; 8]);
    for (&pos, record) in records {
        for coord in pos.to_array() {
            out.extend_from_slice(&coord.to_le_bytes());
        }
        out.extend_from_slice(&record.stamp);
        out.extend_from_slice(&record.occupied);
    }
    let checksum = FxBuildHasher.hash_one(&out[MAGIC.len() + 8..]);
    out[MAGIC.len()..MAGIC.len() + 8].copy_from_slice(&checksum.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn open(dir: &Path) -> (RegionIndex, FxHashSet<IVec3>) {
        let mut cubes = FxHashSet::default();
        let index = RegionIndex::open(
            dir,
            |_| {},
            |p| {
                cubes.insert(p);
            },
        );
        (index, cubes)
    }

    #[test]
    fn reopens_saved_regions_and_recovers_from_a_stale_or_corrupt_index() {
        let dir = std::env::temp_dir().join(format!("weft-region-index-{}", std::process::id()));
        let path = dir.join("r.-1.0.0.bin");
        let mut region = Region::open(&path).unwrap();
        let first = IVec3::new(-8, 0, 0);
        let second = IVec3::new(-7, 0, 0);
        region.write(0, b"one").unwrap();
        let (mut index, cubes) = open(&dir);
        assert!(index.dirty);
        assert_eq!(cubes, [first].into_iter().collect());

        let pos = IVec3::new(-1, 0, 0);
        index.changed(pos);
        index.flush(&dir, |_| Some(&region));
        drop(region);
        let (index, cubes) = open(&dir);
        assert!(!index.dirty);
        assert_eq!(cubes, [first].into_iter().collect());

        // Simulate an interrupted save after the region write but before the index was flushed.
        Region::open(&path).unwrap().write(1, b"two").unwrap();
        let (mut index, cubes) = open(&dir);
        assert!(index.dirty);
        assert_eq!(cubes, [first, second].into_iter().collect());
        index.flush(&dir, |_| None);

        let mut corrupt = fs::OpenOptions::new()
            .append(true)
            .open(dir.join(NAME))
            .unwrap();
        corrupt.write_all(b"trailing").unwrap();
        drop(corrupt);
        let (mut index, cubes) = open(&dir);
        assert!(index.dirty);
        assert_eq!(cubes, [first, second].into_iter().collect());
        index.flush(&dir, |_| None);

        fs::remove_file(path).unwrap();
        let (_, cubes) = open(&dir);
        assert!(cubes.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
