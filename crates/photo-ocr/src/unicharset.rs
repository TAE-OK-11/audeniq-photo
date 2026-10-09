//! `UNICHARSET` (text format) and `UnicharCompress` (the recoder that maps
//! unichars to sequences of network output codes, e.g. Hangul jamo).

use crate::reader::Reader;
use crate::{Error, Result};
use std::collections::HashMap;

pub(crate) const UNICHAR_SPACE: i32 = 0;
pub(crate) const INVALID_UNICHAR_ID: i32 = -1;

const ISALPHA: u32 = 0x1;
const ISLOWER: u32 = 0x2;
const ISUPPER: u32 = 0x4;
const ISDIGIT: u32 = 0x8;
const ISPUNCT: u32 = 0x10;

#[derive(Debug, Clone)]
pub(crate) struct Unichar {
    pub(crate) text: String,
    pub(crate) props: u32,
    pub(crate) script: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Unicharset {
    pub(crate) chars: Vec<Unichar>,
    scripts: Vec<String>,
    han: usize,
    thai: usize,
    hangul: usize,
    hiragana: usize,
    katakana: usize,
}

impl Unicharset {
    pub(crate) fn parse(data: &[u8]) -> Result<Unicharset> {
        let text = String::from_utf8_lossy(data);
        let mut lines = text.split('\n');
        let n: usize = lines
            .next()
            .and_then(|l| l.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .ok_or(Error::Model("bad unicharset header"))?;
        if n > 100_000 {
            return Err(Error::Model("unicharset too large"));
        }
        let mut set = Unicharset::default();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for _ in 0..n {
            let line = lines.next().ok_or(Error::Model("truncated unicharset"))?;
            let mut f = line.split_whitespace();
            let unichar = f.next().ok_or(Error::Model("bad unicharset line"))?;
            let props = f
                .next()
                .and_then(|p| u32::from_str_radix(p, 16).ok())
                .ok_or(Error::Model("bad unichar properties"))?;
            // The script is the first field after the optional metrics.
            let rest: Vec<&str> = f.collect();
            let script = match rest.first() {
                Some(m) if m.contains(',') => rest.get(1).copied(),
                Some(s) => Some(*s),
                None => None,
            }
            .unwrap_or("NULL");
            let repr = if unichar == "NULL" { " " } else { unichar };
            if seen.contains_key(repr) {
                return Err(Error::Model("duplicate unichar"));
            }
            // unichar_insert assigns the null script first.
            if set.chars.is_empty() {
                set.add_script("NULL");
            }
            let script_id = set.add_script(script);
            seen.insert(repr.to_string(), set.chars.len());
            set.chars.push(Unichar {
                text: repr.to_string(),
                props,
                script: script_id,
            });
        }
        let id =
            |set: &Unicharset, name: &str| set.scripts.iter().position(|s| s == name).unwrap_or(0);
        set.han = id(&set, "Han");
        set.thai = id(&set, "Thai");
        set.hangul = id(&set, "Hangul");
        set.hiragana = id(&set, "Hiragana");
        set.katakana = id(&set, "Katakana");
        Ok(set)
    }

    fn add_script(&mut self, name: &str) -> usize {
        if let Some(i) = self.scripts.iter().position(|s| s == name) {
            return i;
        }
        self.scripts.push(name.to_string());
        self.scripts.len() - 1
    }

    pub(crate) fn len(&self) -> usize {
        self.chars.len()
    }

    pub(crate) fn is_digit(&self, id: i32) -> bool {
        self.get(id).is_some_and(|u| u.props & ISDIGIT != 0)
    }

    pub(crate) fn is_alpha(&self, id: i32) -> bool {
        self.get(id).is_some_and(|u| u.props & ISALPHA != 0)
    }

    #[allow(dead_code)]
    pub(crate) fn is_punct(&self, id: i32) -> bool {
        self.get(id).is_some_and(|u| u.props & ISPUNCT != 0)
    }

    #[allow(dead_code)]
    pub(crate) fn is_cased(&self, id: i32) -> bool {
        self.get(id)
            .is_some_and(|u| u.props & (ISLOWER | ISUPPER) != 0)
    }

    pub(crate) fn is_upper(&self, id: i32) -> bool {
        self.get(id).is_some_and(|u| u.props & ISUPPER != 0)
    }

    pub(crate) fn is_lower(&self, id: i32) -> bool {
        self.get(id).is_some_and(|u| u.props & ISLOWER != 0)
    }

    fn get(&self, id: i32) -> Option<&Unichar> {
        usize::try_from(id).ok().and_then(|i| self.chars.get(i))
    }

    pub(crate) fn text(&self, id: i32) -> &str {
        self.get(id).map_or("", |u| u.text.as_str())
    }

    /// `UNICHARSET::IsSpaceDelimited`.
    pub(crate) fn is_space_delimited(&self, id: i32) -> bool {
        if id == INVALID_UNICHAR_ID {
            return true;
        }
        let s = self.get(id).map_or(0, |u| u.script);
        s != self.han
            && s != self.thai
            && s != self.hangul
            && s != self.hiragana
            && s != self.katakana
    }

    /// `Dict::IsSpaceDelimitedLang`.
    pub(crate) fn space_delimited_lang(&self) -> bool {
        !(self.han > 0 || self.katakana > 0 || self.thai > 0)
    }
}

/// `UnicharCompress`.
#[derive(Debug, Default)]
pub(crate) struct Recoder {
    pub(crate) encoder: Vec<Vec<i32>>,
    decoder: HashMap<Vec<i32>, i32>,
    final_codes: HashMap<Vec<i32>, Vec<i32>>,
    next_codes: HashMap<Vec<i32>, Vec<i32>>,
    pub(crate) code_range: i32,
}

impl Recoder {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Recoder> {
        let n = r.u32()? as usize;
        if n > 1_000_000 {
            return Err(Error::Model("recoder too large"));
        }
        let mut encoder = Vec::with_capacity(n);
        for _ in 0..n {
            let _self_normalized = r.i8()?;
            let len = r.i32()?;
            if !(0..=9).contains(&len) {
                return Err(Error::Model("bad recoder code length"));
            }
            let mut code = Vec::with_capacity(len as usize);
            for _ in 0..len {
                code.push(r.i32()?);
            }
            encoder.push(code);
        }
        let code_range = encoder.iter().flatten().copied().max().unwrap_or(-1) + 1;
        let mut rec = Recoder {
            encoder,
            code_range,
            ..Recoder::default()
        };
        rec.setup_decoder();
        Ok(rec)
    }

    fn setup_decoder(&mut self) {
        for (c, code) in self.encoder.iter().enumerate() {
            if code.is_empty() {
                continue;
            }
            self.decoder.insert(code.clone(), c as i32);
            let mut len = code.len() - 1;
            let prefix = code[..len].to_vec();
            match self.final_codes.get_mut(&prefix) {
                None => {
                    self.final_codes.insert(prefix, vec![code[len]]);
                    while len > 0 {
                        len -= 1;
                        let prefix = code[..len].to_vec();
                        match self.next_codes.get_mut(&prefix) {
                            None => {
                                self.next_codes.insert(prefix, vec![code[len]]);
                            }
                            Some(list) => {
                                if !list.contains(&code[len]) {
                                    list.push(code[len]);
                                }
                                break;
                            }
                        }
                    }
                }
                Some(list) => {
                    if !list.contains(&code[len]) {
                        list.push(code[len]);
                    }
                }
            }
        }
    }

    pub(crate) fn decode(&self, code: &[i32]) -> i32 {
        if code.is_empty() || code.len() > 9 {
            return INVALID_UNICHAR_ID;
        }
        self.decoder
            .get(code)
            .copied()
            .unwrap_or(INVALID_UNICHAR_ID)
    }

    pub(crate) fn final_codes(&self, prefix: &[i32]) -> Option<&Vec<i32>> {
        self.final_codes.get(prefix)
    }

    pub(crate) fn next_codes(&self, prefix: &[i32]) -> Option<&Vec<i32>> {
        self.next_codes.get(prefix)
    }
}
