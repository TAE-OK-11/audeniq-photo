//! One-pass cover inspection: probe, color properties, provenance and QR
//! from a single read of the file (the backend previously ran ffprobe,
//! exiftool twice and zbarimg, each re-reading and re-parsing the image).

use crate::report::{color_report, provenance_fields};
use crate::{Deadline, Probe, Result, guard, probe};
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub struct CoverReport {
    pub probe: Probe,
    /// `IMAGE_COLOR_PROFILE` properties (ExifTool `-j -n -s` shape).
    pub color: Map<String, Value>,
    /// Provenance fields (ExifTool `-j -n -G1 -s` shape).
    pub provenance: Map<String, Value>,
    /// Decoded QR codes, or the reason the scan could not run.
    pub qr: std::result::Result<usize, crate::Error>,
}

pub fn inspect_cover(data: &[u8], deadline: &Deadline) -> Result<CoverReport> {
    guard(|| {
        let probe = probe(data)?;
        let meta = photo_meta::read(data)?;
        let qr = crate::intensity(data, deadline)
            .and_then(|(w, h, gray)| Ok(photo_qr::scan(&gray, w, h, deadline)?.decoded));
        Ok(CoverReport {
            probe,
            color: color_report(&meta),
            provenance: provenance_fields(&meta),
            qr,
        })
    })
}
