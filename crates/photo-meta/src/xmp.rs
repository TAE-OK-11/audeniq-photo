//! XMP: a small namespace-aware XML reader and the RDF walk ExifTool does
//! (simple, lang-alt, list and flattened-structure properties). No DTD or
//! external entity is ever processed.

use crate::{Metadata, Value, latin1};
use std::collections::HashMap;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 200_000;
const MAX_XMP: usize = 4 * 1024 * 1024;
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Debug, Default)]
struct Element {
    ns: String,
    local: String,
    prefix: String,
    attrs: Vec<(String, String, String, String)>, // (ns, local, prefix, value)
    children: Vec<Element>,
    text: String,
}

impl Element {
    fn attr(&self, ns: &str, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.0 == ns && a.1 == local)
            .map(|a| a.3.as_str())
    }
    fn is(&self, ns: &str, local: &str) -> bool {
        self.ns == ns && self.local == local
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
    nodes: usize,
}

type Scope = HashMap<String, String>;

impl<'a> Parser<'a> {
    fn starts(&self, p: &str) -> bool {
        self.s[self.pos..].starts_with(p)
    }

    fn skip_until(&mut self, end: &str) -> Option<()> {
        let i = self.s[self.pos..].find(end)?;
        self.pos += i + end.len();
        Some(())
    }

    /// Skip prolog-like constructs; true if something was skipped.
    fn skip_misc(&mut self) -> Option<bool> {
        if self.starts("<?") {
            self.skip_until("?>")?;
        } else if self.starts("<!--") {
            self.skip_until("-->")?;
        } else if self.starts("<!DOCTYPE") || self.starts("<!doctype") {
            // Skip, including any internal subset; never expand.
            let mut depth = 0i32;
            for (i, c) in self.s[self.pos..].char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    '>' if depth <= 0 => {
                        self.pos += i + 1;
                        return Some(true);
                    }
                    _ => {}
                }
            }
            return None;
        } else {
            return Some(false);
        }
        Some(true)
    }

    fn name(&mut self) -> &'a str {
        let start = self.pos;
        while let Some(c) = self.s[self.pos..].chars().next() {
            if c.is_whitespace() || matches!(c, '/' | '>' | '=') {
                break;
            }
            self.pos += c.len_utf8();
        }
        &self.s[start..self.pos]
    }

    fn ws(&mut self) {
        while let Some(c) = self.s[self.pos..].chars().next() {
            if !c.is_whitespace() {
                break;
            }
            self.pos += c.len_utf8();
        }
    }

    fn element(&mut self, parent: &Scope, depth: usize) -> Option<Element> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return None;
        }
        // At '<'
        self.pos += 1;
        let qname = self.name().to_string();
        let mut raw_attrs = Vec::new();
        let mut scope = parent.clone();
        let self_closing;
        loop {
            self.ws();
            if self.starts("/>") {
                self.pos += 2;
                self_closing = true;
                break;
            }
            if self.starts(">") {
                self.pos += 1;
                self_closing = false;
                break;
            }
            let an = self.name().to_string();
            if an.is_empty() {
                return None;
            }
            self.ws();
            if !self.starts("=") {
                return None;
            }
            self.pos += 1;
            self.ws();
            let q = self.s[self.pos..].chars().next()?;
            if q != '"' && q != '\'' {
                return None;
            }
            self.pos += 1;
            let end = self.s[self.pos..].find(q)?;
            let v = decode_entities(&self.s[self.pos..self.pos + end]);
            self.pos += end + 1;
            if an == "xmlns" {
                scope.insert(String::new(), v);
            } else if let Some(p) = an.strip_prefix("xmlns:") {
                scope.insert(p.to_string(), v);
            } else {
                raw_attrs.push((an, v));
            }
        }
        let resolve = |q: &str, default_ns: bool| -> (String, String, String) {
            match q.split_once(':') {
                Some((p, l)) => {
                    let ns = if p == "xml" {
                        XML_NS.to_string()
                    } else {
                        scope.get(p).cloned().unwrap_or_default()
                    };
                    (ns, l.to_string(), p.to_string())
                }
                None => (
                    if default_ns {
                        scope.get("").cloned().unwrap_or_default()
                    } else {
                        String::new()
                    },
                    q.to_string(),
                    String::new(),
                ),
            }
        };
        let (ns, local, prefix) = resolve(&qname, true);
        let attrs = raw_attrs
            .into_iter()
            .map(|(n, v)| {
                let (ans, al, ap) = resolve(&n, false);
                (ans, al, ap, v)
            })
            .collect();
        let mut el = Element {
            ns,
            local,
            prefix,
            attrs,
            children: Vec::new(),
            text: String::new(),
        };
        if self_closing {
            return Some(el);
        }
        loop {
            if self.pos >= self.s.len() {
                return None;
            }
            if self.starts("</") {
                self.skip_until(">")?;
                return Some(el);
            }
            if self.starts("<![CDATA[") {
                self.pos += 9;
                let end = self.s[self.pos..].find("]]>")?;
                el.text.push_str(&self.s[self.pos..self.pos + end]);
                self.pos += end + 3;
                continue;
            }
            if self.skip_misc()? {
                continue;
            }
            if self.starts("<") {
                let child = self.element(&scope, depth + 1)?;
                el.children.push(child);
                continue;
            }
            let end = self.s[self.pos..]
                .find('<')
                .map_or(self.s.len(), |i| self.pos + i);
            el.text.push_str(&decode_entities(&self.s[self.pos..end]));
            self.pos = end;
        }
    }

    fn document(&mut self) -> Option<Vec<Element>> {
        let mut roots = Vec::new();
        let scope = Scope::new();
        while self.pos < self.s.len() {
            if self.skip_misc()? {
                continue;
            }
            match self.s[self.pos..].find('<') {
                Some(i) => {
                    self.pos += i;
                    if self.skip_misc()? {
                        continue;
                    }
                    roots.push(self.element(&scope, 0)?);
                }
                None => break,
            }
        }
        Some(roots)
    }
}

fn group_prefix(ns: &str, declared: &str) -> String {
    let p = match ns {
        "http://purl.org/dc/elements/1.1/" => "dc",
        "http://ns.adobe.com/xap/1.0/" => "xmp",
        "http://ns.adobe.com/xap/1.0/mm/" => "xmpMM",
        "http://ns.adobe.com/xap/1.0/rights/" => "xmpRights",
        "http://ns.adobe.com/xap/1.0/bj/" => "xmpBJ",
        "http://ns.adobe.com/xap/1.0/t/pg/" => "xmpTPg",
        "http://ns.adobe.com/tiff/1.0/" => "tiff",
        "http://ns.adobe.com/exif/1.0/" => "exif",
        "http://cipa.jp/exif/1.0/" => "exifEX",
        "http://ns.adobe.com/exif/1.0/aux/" => "aux",
        "http://ns.adobe.com/photoshop/1.0/" => "photoshop",
        "http://ns.adobe.com/camera-raw-settings/1.0/" => "crs",
        "http://ns.adobe.com/pdf/1.3/" => "pdf",
        "http://ns.adobe.com/xmp/1.0/DynamicMedia/" => "xmpDM",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" => "iptcCore",
        "http://iptc.org/std/Iptc4xmpExt/2008-02-29/" => "iptcExt",
        "http://ns.useplus.org/ldf/xmp/1.0/" => "plus",
        "http://ns.google.com/photos/1.0/image/" => "GImage",
        "http://ns.google.com/photos/1.0/camera/" => "GCamera",
        "http://ns.adobe.com/photoshop/1.0/panorama-profile" => "GPano",
        _ => declared,
    };
    format!("XMP-{p}")
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn is_rdf_or_xml(ns: &str) -> bool {
    ns == RDF || ns == XML_NS
}

struct Walk<'m> {
    m: &'m mut Metadata,
}

impl Walk<'_> {
    fn description(&mut self, d: &Element) {
        for a in &d.attrs {
            if is_rdf_or_xml(&a.0) || a.0.is_empty() {
                continue;
            }
            self.m.push(
                group_prefix(&a.0, &a.2),
                ucfirst(&a.1),
                Value::Text(a.3.clone()),
            );
        }
        for p in &d.children {
            let group = group_prefix(&p.ns, &p.prefix);
            self.property(p, &group, &ucfirst(&p.local));
        }
    }

    fn property(&mut self, p: &Element, group: &str, name: &str) {
        if let Some(r) = p.attr(RDF, "resource") {
            self.m.push(group, name, Value::Text(r.to_string()));
            return;
        }
        let container = p
            .children
            .iter()
            .find(|c| c.ns == RDF && matches!(c.local.as_str(), "Alt" | "Bag" | "Seq"));
        if let Some(c) = container {
            let items: Vec<&Element> = c.children.iter().filter(|li| li.is(RDF, "li")).collect();
            if c.local == "Alt" && items.iter().any(|li| li.attr(XML_NS, "lang").is_some()) {
                for li in items {
                    let lang = li.attr(XML_NS, "lang").unwrap_or("x-default");
                    let n = if lang.eq_ignore_ascii_case("x-default") {
                        name.to_string()
                    } else {
                        format!("{name}-{lang}")
                    };
                    self.m
                        .push(group, n, Value::Text(li.text.trim().to_string()));
                }
                return;
            }
            let mut simple = Vec::new();
            for li in items {
                if li.children.is_empty() && li.attrs.iter().all(|a| is_rdf_or_xml(&a.0)) {
                    simple.push(li.text.trim().to_string());
                } else {
                    self.structure(li, group, name);
                }
            }
            if !simple.is_empty() {
                self.m.push(group, name, Value::List(simple));
            }
            return;
        }
        let is_struct = p.attr(RDF, "parseType") == Some("Resource")
            || p.children.iter().any(|c| c.is(RDF, "Description"))
            || (!p.children.is_empty())
            || p.attrs
                .iter()
                .any(|a| !is_rdf_or_xml(&a.0) && !a.0.is_empty());
        if is_struct {
            self.structure(p, group, name);
            return;
        }
        self.m
            .push(group, name, Value::Text(p.text.trim().to_string()));
    }

    /// Flatten a structure: field tag name = parent name + field name.
    fn structure(&mut self, s: &Element, group: &str, name: &str) {
        let target = s
            .children
            .iter()
            .find(|c| c.is(RDF, "Description"))
            .unwrap_or(s);
        for a in &target.attrs {
            if is_rdf_or_xml(&a.0) || a.0.is_empty() {
                continue;
            }
            self.m.push(
                group,
                format!("{name}{}", ucfirst(&a.1)),
                Value::Text(a.3.clone()),
            );
        }
        for f in &target.children {
            if f.ns == RDF {
                continue;
            }
            self.property(f, group, &format!("{name}{}", ucfirst(&f.local)));
        }
    }

    fn find_rdf(&mut self, e: &Element) {
        if e.is(RDF, "RDF") {
            for d in &e.children {
                if d.is(RDF, "Description") {
                    self.description(d);
                }
            }
            return;
        }
        for c in &e.children {
            self.find_rdf(c);
        }
    }
}

pub(crate) fn read(data: &[u8], m: &mut Metadata) {
    if data.len() > MAX_XMP {
        m.warn("XMP too large");
        return;
    }
    let text = match data {
        [0xFE, 0xFF, rest @ ..] => String::from_utf16_lossy(
            &rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        ),
        [0xFF, 0xFE, rest @ ..] => String::from_utf16_lossy(
            &rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        ),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => match std::str::from_utf8(data) {
            Ok(s) => s.to_string(),
            Err(_) => latin1(data),
        },
    };
    let text = text.trim_end_matches(['\0', ' ', '\n', '\r', '\t']);
    let mut p = Parser {
        s: text,
        pos: 0,
        nodes: 0,
    };
    match p.document() {
        Some(roots) => {
            let mut w = Walk { m };
            for r in &roots {
                w.find_rdf(r);
            }
        }
        None => m.warn("invalid XMP"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_alt_list_struct_and_attributes() {
        let x = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/"
  xmlns:Iptc4xmpExt="http://iptc.org/std/Iptc4xmpExt/2008-02-29/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
  xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/" xmlns:stEvt="http://ns.adobe.com/xap/1.0/sType/ResourceEvent#"
  xmp:CreatorTool="Midjourney &amp; co" tiff:Orientation="6">
 <dc:description><rdf:Alt><rdf:li xml:lang="x-default">Made <![CDATA[with]]> AI</rdf:li><rdf:li xml:lang="ko-KR">설명</rdf:li></rdf:Alt></dc:description>
 <dc:subject><rdf:Bag><rdf:li>a</rdf:li><rdf:li>b</rdf:li></rdf:Bag></dc:subject>
 <Iptc4xmpExt:DigitalSourceType>http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia</Iptc4xmpExt:DigitalSourceType>
 <xmpMM:History><rdf:Seq><rdf:li stEvt:action="saved" stEvt:softwareAgent="Adobe Photoshop"/></rdf:Seq></xmpMM:History>
</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;
        let mut m = Metadata::default();
        read(x.as_bytes(), &mut m);
        let get = |g: &str, n: &str| {
            m.tags
                .iter()
                .find(|t| t.group == g && t.name == n)
                .map(|t| t.value.clone())
        };
        assert_eq!(
            get("XMP-xmp", "CreatorTool"),
            Some(Value::Text("Midjourney & co".into()))
        );
        assert_eq!(
            get("XMP-tiff", "Orientation"),
            Some(Value::Text("6".into()))
        );
        assert_eq!(
            get("XMP-dc", "Description"),
            Some(Value::Text("Made with AI".into()))
        );
        assert_eq!(
            get("XMP-dc", "Description-ko-KR"),
            Some(Value::Text("설명".into()))
        );
        assert_eq!(
            get("XMP-dc", "Subject"),
            Some(Value::List(vec!["a".into(), "b".into()]))
        );
        assert!(get("XMP-iptcExt", "DigitalSourceType").is_some());
        assert_eq!(
            get("XMP-xmpMM", "HistorySoftwareAgent"),
            Some(Value::Text("Adobe Photoshop".into()))
        );
        assert!(get("XMP-xmpMM", "Software").is_none());
    }

    #[test]
    fn hostile_xml_is_bounded() {
        let mut m = Metadata::default();
        read(
            b"<!DOCTYPE x [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;\">]><x>&b;</x>",
            &mut m,
        );
        let deep = "<a>".repeat(10_000);
        read(deep.as_bytes(), &mut m);
        read(b"<a b='unterminated", &mut m);
        assert!(m.tags.is_empty());
    }
}
