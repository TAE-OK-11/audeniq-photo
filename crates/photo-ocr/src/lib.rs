//! OCR for Audeniq: a Rust port of Tesseract's LSTM recognizer (Tesseract
//! 5.3, Apache-2.0) for the `tessdata_fast` models the backend used
//! (`eng`, `kor`).
//!
//! [`ocr_tsv`] reproduces `tesseract <image> stdout -l eng+kor --psm 11 tsv`
//! byte for byte: thresholding, sparse-text layout ([`page`]), textline
//! and word segmentation, per-word LSTM recognition with language retry
//! ([`recog`]) and the TSV report. [`Model::recognize_line`] alone matches
//! `--psm 13` (one raw text line).
// Safe code throughout, except the one call into the AVX2-compiled LSTM
// kernel after runtime CPU detection (`lstm::simd`).
#![deny(unsafe_code)]

mod beam;
mod dict;
mod lstm;
pub mod page;
pub mod pix;
mod reader;
pub mod recog;
mod tessdata;
mod unicharset;

pub use beam::Word;
pub use pix::{Gray, Pix};

use dict::{Dawg, DawgType, Dict, NUMBER_PERM, PUNC_PERM, SYSTEM_DAWG_PERM};
use lstm::Network;
use lstm::io::NetIo;
use lstm::rand::TRand;
use reader::Reader;
use std::fmt;
use unicharset::{Recoder, Unicharset};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The model file is malformed or uses unsupported features.
    Model(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Model(m) => write!(f, "OCR model: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

const TF_INT_MODE: i32 = 1;
const TF_COMPRESS_UNICHARSET: i32 = 64;
/// `kDictRatio`, `kCertOffset`, `kWorstDictCertainty / kCertaintyScale`.
const DICT_RATIO: f64 = 2.25;
const CERT_OFFSET: f64 = -0.085;
const MAX_INPUT_HEIGHT: i32 = 48;
/// `invert_threshold` default.
const INVERT_THRESHOLD: f32 = 0.7;

/// The embedded `tessdata_fast` models (Apache-2.0; see `models/README.md`).
pub mod models {
    pub static ENG: &[u8] = include_bytes!("../models/eng.traineddata");
    pub static KOR: &[u8] = include_bytes!("../models/kor.traineddata");
}

/// A loaded LSTM language model (`.traineddata`).
pub struct Model {
    net: Network,
    set: Unicharset,
    recoder: Recoder,
    dict: Option<Dict>,
    null_char: i32,
    int_mode: bool,
    sample_iteration: i32,
}

/// Words recognized on one line, with their x extent in the line image.
#[derive(Clone, Debug, PartialEq)]
pub struct LineWord {
    pub word: Word,
    pub left: i32,
    pub right: i32,
}

impl Model {
    pub fn load(traineddata: &[u8]) -> Result<Model> {
        let td = tessdata::Tessdata::parse(traineddata)?;
        let lstm_data = td
            .get(tessdata::LSTM)
            .ok_or(Error::Model("no LSTM model"))?;
        let mut r = Reader::new(lstm_data);
        let net = Network::read(&mut r)?;
        let separate =
            td.get(tessdata::LSTM_UNICHARSET).is_some() && td.get(tessdata::LSTM_RECODER).is_some();
        let embedded_set = if separate {
            None
        } else {
            Some(read_embedded_unicharset(&mut r)?)
        };
        let _network_str = r.string()?;
        let training_flags = r.i32()?;
        let _training_iteration = r.i32()?;
        let sample_iteration = r.i32()?;
        let null_char = r.i32()?;
        let _adam_beta = r.f32()?;
        let _learning_rate = r.f32()?;
        let _momentum = r.f32()?;
        let recoding = training_flags & TF_COMPRESS_UNICHARSET != 0;
        let (set, recoder) = if let Some(set) = embedded_set {
            let rec = if recoding {
                Recoder::read(&mut r)?
            } else {
                pass_through(&set)
            };
            (set, rec)
        } else {
            let set = Unicharset::parse(td.get(tessdata::LSTM_UNICHARSET).expect("checked"))?;
            let rec = if recoding {
                Recoder::read(&mut Reader::new(
                    td.get(tessdata::LSTM_RECODER).expect("checked"),
                ))?
            } else {
                pass_through(&set)
            };
            (set, rec)
        };
        if null_char < 0
            || null_char as usize >= net.num_outputs
            || recoder.code_range as usize > net.num_outputs
        {
            return Err(Error::Model(
                "output layer does not match the character set",
            ));
        }
        let mut dawgs = Vec::new();
        for (which, kind, perm) in [
            (tessdata::LSTM_PUNC_DAWG, DawgType::Punctuation, PUNC_PERM),
            (tessdata::LSTM_SYSTEM_DAWG, DawgType::Word, SYSTEM_DAWG_PERM),
            (tessdata::LSTM_NUMBER_DAWG, DawgType::Number, NUMBER_PERM),
        ] {
            if let Some(d) = td.get(which) {
                dawgs.push(Dawg::read(d, kind, perm)?);
            }
        }
        Ok(Model {
            int_mode: training_flags & TF_INT_MODE != 0,
            net,
            set,
            recoder,
            dict: Dict::new(dawgs),
            null_char,
            sample_iteration,
        })
    }

    fn seeded(&self, rand: &mut TRand) {
        rand.set_seed((i64::from(self.sample_iteration) * 0x1000_0001) as u64);
        rand.int_rand();
    }

    /// `LSTMRecognizer::RecognizeLine` on a grey line image (the
    /// `GetRectImage` output) followed by `ExtractBestPathAsWords` and the
    /// `SearchWords` certainty scaling.
    pub fn recognize_line(&self, line: &Pix, rand: &mut OcrRandom) -> Vec<LineWord> {
        let (words, scale_factor) = self.recognize_raw(line, rand);
        words
            .into_iter()
            .filter(|w| !w.all_spaces)
            .map(|w| LineWord {
                left: (w.start_t as f32 * scale_factor).floor() as i32,
                right: (w.end_t as f32 * scale_factor).ceil() as i32,
                word: w,
            })
            .collect()
    }

    /// The decoded words of a line image (all-space words included) and
    /// the factor from network time steps to image x.
    pub(crate) fn recognize_raw(&self, line: &Pix, rand: &mut OcrRandom) -> (Vec<Word>, f32) {
        let rand = &mut rand.0;
        self.seeded(rand);
        let min_width = self.net.x_scale();
        let mut target = self.net.num_inputs;
        if target == 0 {
            target = (line.height() as i32).min(MAX_INPUT_HEIGHT);
        }
        if line.height() == 0 || line.width() == 0 {
            return (Vec::new(), 0.0);
        }
        let im_factor = target as f32 / line.height() as f32;
        // `PreScale` scales the original (possibly colour) image; the grey
        // conversion happens in `PreparePixInput`.
        let mut scaled = line.scale(im_factor, im_factor);
        let pix = scaled.to_gray();
        if (pix.width as i32) < min_width || (pix.height as i32) < min_width {
            return (Vec::new(), 0.0);
        }
        let scale_factor = min_width as f32 / im_factor;
        self.seeded(rand);
        let inputs = self.prepare(&pix, rand);
        let mut outputs = self.net.forward(&inputs, rand);
        let (_, pos_mean) = self.output_stats(&outputs);
        if pos_mean < INVERT_THRESHOLD {
            self.seeded(rand);
            // `pixInvert` acts on the scaled (possibly colour) image; the
            // grey conversion follows in `PreparePixInput`.
            scaled.invert();
            let inv_inputs = self.prepare(&scaled.to_gray(), rand);
            let inv_outputs = self.net.forward(&inv_inputs, rand);
            let (_, inv_mean) = self.output_stats(&inv_outputs);
            if inv_mean > pos_mean {
                outputs = inv_outputs;
            }
        }
        let rows: Vec<&[f32]> = (0..outputs.width()).map(|t| outputs.frow(t)).collect();
        let mut search =
            beam::Search::new(&self.recoder, self.null_char, self.dict.as_ref(), &self.set);
        let worst = f64::from(-25.0f32 / 7.0f32);
        search.decode(&rows, DICT_RATIO, CERT_OFFSET, worst);
        (search.words(), scale_factor)
    }

    /// `Input::PreparePixInput` + `NetworkIO::FromPix`.
    fn prepare(&self, pix: &Gray, rand: &mut TRand) -> NetIo {
        let mut target_height = self.net.input_height;
        if target_height == 1 {
            target_height = self.net.input_depth;
        }
        let scaled;
        let pix = if target_height != 0 && target_height != pix.height as i32 {
            let f = target_height as f32 / pix.height as f32;
            scaled = pix::scale(pix, f, f);
            &scaled
        } else {
            pix
        };
        let (black, white) = black_white(pix);
        let mut contrast = (white - black) / 2.0;
        if contrast <= 0.0 {
            contrast = 1.0;
        }
        let height = if self.net.input_height != 0 {
            self.net.input_height
        } else {
            pix.height as i32
        };
        let mut map = lstm::io::StrideMap::default();
        map.set_stride(&[(height, pix.width as i32)]);
        let mut io = NetIo::default();
        io.resize_to_map(self.int_mode, map, self.net.input_depth.max(1) as usize);
        let set_pixel = |io: &mut NetIo, t: usize, p: u8| {
            let fp: f32 = (f32::from(p) - black) / contrast - 1.0;
            if io.int_mode {
                io.i[t * io.nf] = lstm::io::round_f32(128.0 * fp).clamp(-127, 127) as i8;
            } else {
                io.f[t * io.nf] = fp;
            }
        };
        let target_w = io.map.size(lstm::io::WIDTH) as usize;
        let target_h = io.map.size(lstm::io::HEIGHT) as usize;
        let width = pix.width.min(target_w);
        let mut t = 0;
        for y in 0..target_h {
            let mut x = 0;
            if y < pix.height {
                while x < width {
                    set_pixel(&mut io, t, pix.get(x, y));
                    x += 1;
                    t += 1;
                }
            }
            while x < target_w {
                let nf = io.nf;
                io.randomize(t, 0, nf, rand);
                x += 1;
                t += 1;
            }
        }
        io
    }

    /// `LSTMRecognizer::OutputStats` → (min, mean).
    fn output_stats(&self, out: &NetIo) -> (f32, f32) {
        let mut buckets = [0i64; 128];
        let mut total = 0i64;
        for t in 0..out.width() {
            let row = out.frow(t);
            let mut best = -1i32;
            let mut best_score = -f32::MAX;
            for (i, &v) in row.iter().enumerate() {
                if v > best_score {
                    best_score = v;
                    best = i as i32;
                }
            }
            if best != self.null_char {
                let v = (127.0 * best_score) as i32;
                buckets[v.clamp(0, 127) as usize] += 1;
                total += 1;
            }
        }
        if total == 0 {
            return (0.0, 0.0);
        }
        let min = buckets.iter().position(|&b| b > 0).unwrap_or(0) as f32 / 127.0;
        let sum: i64 = buckets.iter().enumerate().map(|(i, &b)| i as i64 * b).sum();
        let mean = ((sum as f64 / total as f64) / 127.0) as f32;
        (min, mean)
    }
}

/// `tesseract <image> stdout --psm 11 tsv` with the given languages (in
/// `-l` order, e.g. eng then kor): sparse-text page layout, word
/// recognition and Tesseract's TSV report.
pub fn ocr_tsv(pix: &Pix, models: &[&Model]) -> String {
    ocr_tsv_until(pix, models, &|| false).expect("never stopped")
}

/// [`ocr_tsv`] that gives up (`None`) once `stop` returns true.
pub fn ocr_tsv_until(pix: &Pix, models: &[&Model], stop: &dyn Fn() -> bool) -> Option<String> {
    let blocks = page_blocks(pix);
    if stop() {
        return None;
    }
    let rec = recog::recognize_page(pix, &blocks, models, stop)?;
    let header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";
    Some(format!(
        "{header}{}",
        recog::tsv(&rec, pix.width() as i32, pix.height() as i32)
    ))
}

/// Page segmentation for sparse text (`--psm 11`): the blocks of words.
pub fn page_blocks(pix: &Pix) -> Vec<page::wordseg::TextBlock> {
    let mut bin = page::thresh::threshold(pix);
    let lines = page::linefind::find_and_remove_lines(70, &mut bin);
    let photo = page::imagefind::find_images(&bin);
    let mut blobs = page::blobbox::Blobs::default();
    let Some(mut tb) = page::blobbox::find_components(&bin, &mut blobs) else {
        return Vec::new();
    };
    let mut resolution = 70;
    let res = page::detlinefit::int_cast_rounded(f64::from(tb.line_size) * 10.0);
    if res > resolution && res < 2400 {
        resolution = res;
    }
    if tb.line_size < 2.0 {
        return Vec::new();
    }
    let (w, h) = (bin.width as i32, bin.height as i32);
    let mut layout = page::layout::Layout::new(
        tb.line_size as i32,
        page::geom::ICoord::new(0, 0),
        page::geom::ICoord::new(w, h),
        resolution,
        lines.vertical_x,
        lines.vertical_y,
    );
    layout.setup_and_filter_noise(&mut blobs, &photo, &mut tb);
    let (blocks, diacritics) = layout.find_blocks(&mut blobs, &mut tb);
    page::tordmain::textord_page(blocks, &diacritics, &mut blobs)
}

/// The recognizer's random generator; Tesseract keeps one per language
/// model and reseeds it per line, so a fresh value per call is equivalent.
#[derive(Default)]
pub struct OcrRandom(TRand);

fn read_embedded_unicharset(_r: &mut Reader<'_>) -> Result<Unicharset> {
    Err(Error::Model(
        "models with embedded character sets are not supported",
    ))
}

fn pass_through(set: &Unicharset) -> Recoder {
    let mut bytes = Vec::new();
    let n = set.len() as u32;
    bytes.extend_from_slice(&n.to_le_bytes());
    for u in 0..n {
        bytes.push(1);
        bytes.extend_from_slice(&1i32.to_le_bytes());
        bytes.extend_from_slice(&(u as i32).to_le_bytes());
    }
    Recoder::read(&mut Reader::new(&bytes)).expect("pass-through recoder")
}

/// `ComputeBlackWhite`: 25th percentile of local minima and 75th of local
/// maxima along the middle row (`STATS::ile`).
fn black_white(pix: &Gray) -> (f32, f32) {
    let mut mins = [0i32; 256];
    let mut maxes = [0i32; 256];
    let (w, h) = (pix.width, pix.height);
    if w >= 3 {
        let y = h / 2;
        let mut prev = i32::from(pix.get(0, y));
        let mut curr = i32::from(pix.get(1, y));
        for x in 1..w - 1 {
            let next = i32::from(pix.get(x + 1, y));
            if (curr < prev && curr <= next) || (curr <= prev && curr < next) {
                mins[curr as usize] += 1;
            }
            if (curr > prev && curr >= next) || (curr >= prev && curr > next) {
                maxes[curr as usize] += 1;
            }
            prev = curr;
            curr = next;
        }
    }
    if mins.iter().all(|&c| c == 0) {
        mins[0] = 1;
    }
    if maxes.iter().all(|&c| c == 0) {
        maxes[255] = 1;
    }
    (ile(&mins, 0.25) as f32, ile(&maxes, 0.75) as f32)
}

fn ile(buckets: &[i32; 256], frac: f64) -> f64 {
    let total: i32 = buckets.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let target = (frac * f64::from(total)).clamp(1.0, f64::from(total));
    let mut sum = 0i32;
    let mut index = 0usize;
    while index < 256 && f64::from(sum) < target {
        sum += buckets[index];
        index += 1;
    }
    if index > 0 {
        index as f64 - (f64::from(sum) - target) / f64::from(buckets[index - 1])
    } else {
        0.0
    }
}
