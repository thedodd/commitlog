use std::fs;
use std::io;
use std::mem::swap;

use std::collections::BTreeMap;
use super::index::*;
use super::segment::*;
use super::LogOptions;

pub struct FileSet {
    active: (Index, Segment),
    closed: BTreeMap<u64, (Index, Segment)>,
    opts: LogOptions,
}

impl FileSet {
    pub fn load_log(opts: LogOptions) -> io::Result<FileSet> {
        let mut segments = BTreeMap::new();
        let mut indexes = BTreeMap::new();

        let files = fs::read_dir(&opts.log_dir)?
            // ignore Err results
            .filter_map(|e| e.ok())
            // ignore directories
            .filter(|e| e.metadata().map(|m| m.is_file()).unwrap_or(false));

        for f in files {
            match f.path().extension() {
                Some(ext) if SEGMENT_FILE_NAME_EXTENSION.eq(ext) => {
                    let segment = match Segment::open(f.path(), opts.log_max_bytes) {
                        Ok(seg) => seg,
                        Err(e) => {
                            error!("Unable to open segment {:?}: {}", f.path(), e);
                            return Err(e);
                        }
                    };

                    let offset = segment.starting_offset();
                    segments.insert(offset, segment);
                }
                Some(ext) if INDEX_FILE_NAME_EXTENSION.eq(ext) => {
                    let index = match Index::open(f.path()) {
                        Ok(ind) => ind,
                        Err(e) => {
                            error!("Unable to open index {:?}: {}", f.path(), e);
                            return Err(e);
                        }
                    };

                    let offset = index.starting_offset();
                    indexes.insert(offset, index);
                    // TODO: fix missing index updates (crash before write to index)
                }
                _ => {}
            }
        }

        // pair up the index and segments (there should be an index per segment)
        let mut closed = segments.into_iter()
            .map(move |(i, s)| {
                match indexes.remove(&i) {
                    Some(v) => (i, (v, s)),
                    None => {
                        // TODO: create the index from the segment
                        panic!("No index found for segment starting at {}", i);
                    }
                }
            })
            .collect::<BTreeMap<u64, (Index, Segment)>>();

        // try to reuse the last index if it is not full. otherwise, open a new index
        // at the correct offset
        let last_entry = closed.keys().next_back().cloned();
        let (ind, seg) = match last_entry {
            Some(off) => {
                info!("Reusing index and segment starting at offset {}", off);
                closed.remove(&off).unwrap()
            }
            None => {
                info!("Starting new index and segment at offset 0");
                let ind = Index::new(&opts.log_dir, 0, opts.index_max_bytes)?;
                let seg = Segment::new(&opts.log_dir, 0, opts.log_max_bytes)?;
                (ind, seg)
            }
        };

        // mark all closed indexes as readonly (indexes are not opened as readonly)
        for &mut (ref mut ind, _) in closed.values_mut() {
            ind.set_readonly()?;
        }

        Ok(FileSet {
            active: (ind, seg),
            closed: closed,
            opts: opts,
        })
    }

    pub fn active_segment_mut(&mut self) -> &mut Segment {
        &mut self.active.1
    }

    pub fn active_index_mut(&mut self) -> &mut Index {
        &mut self.active.0
    }

    pub fn active_index(&self) -> &Index {
        &self.active.0
    }

    pub fn find(&self, offset: u64) -> Option<&(Index, Segment)> {
        let active_seg_start_off = self.active.0.starting_offset();
        if offset >= active_seg_start_off {
            trace!("Index is contained in the active index for offset {}",
                   offset);
            Some(&self.active)
        } else {
            self.closed.range(..(offset + 1)).next_back().map(|p| p.1)
        }
    }

    pub fn roll_segment(&mut self) -> io::Result<()> {
        self.active.0.set_readonly()?;
        self.active.1.flush_sync()?;

        let next_offset = self.active.0.next_offset();

        info!("Starting new segment and index at offset {}", next_offset);

        // set the segment and index to the new active index/seg
        let mut p = {
            let seg = Segment::new(&self.opts.log_dir, next_offset, self.opts.log_max_bytes)?;
            let ind = Index::new(&self.opts.log_dir, next_offset, self.opts.index_max_bytes)?;
            (ind, seg)
        };
        swap(&mut p, &mut self.active);
        self.closed.insert(p.1.starting_offset(), p);
        Ok(())
    }

    pub fn log_options(&self) -> &LogOptions {
        &self.opts
    }
}
