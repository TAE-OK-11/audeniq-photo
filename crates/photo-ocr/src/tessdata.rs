//! `.traineddata` container (`TessdataManager`): a table of component offsets.

use crate::reader::Reader;
use crate::{Error, Result};

pub(crate) const LSTM: usize = 17;
pub(crate) const LSTM_PUNC_DAWG: usize = 18;
pub(crate) const LSTM_SYSTEM_DAWG: usize = 19;
pub(crate) const LSTM_NUMBER_DAWG: usize = 20;
pub(crate) const LSTM_UNICHARSET: usize = 21;
pub(crate) const LSTM_RECODER: usize = 22;
const NUM_ENTRIES: usize = 24;

pub(crate) struct Tessdata<'a> {
    entries: [Option<&'a [u8]>; NUM_ENTRIES],
}

impl<'a> Tessdata<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let n = r.u32()? as usize;
        if n > 1000 {
            return Err(Error::Model("not a traineddata file"));
        }
        let mut offsets = Vec::with_capacity(n);
        for _ in 0..n {
            offsets.push(r.i64()?);
        }
        let mut entries = [None; NUM_ENTRIES];
        for i in 0..n.min(NUM_ENTRIES) {
            let start = offsets[i];
            if start < 0 {
                continue;
            }
            // Size runs to the next present entry, else to the end.
            let end = offsets[i + 1..]
                .iter()
                .copied()
                .find(|&o| o != -1)
                .unwrap_or(data.len() as i64);
            let (s, e) = (start as usize, end as usize);
            if s > e || e > data.len() {
                return Err(Error::Model("bad traineddata offsets"));
            }
            entries[i] = Some(&data[s..e]);
        }
        Ok(Tessdata { entries })
    }

    pub(crate) fn get(&self, which: usize) -> Option<&'a [u8]> {
        self.entries[which].filter(|e| !e.is_empty())
    }
}
