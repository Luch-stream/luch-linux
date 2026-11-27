//! Speaker notes of a presentation, one entry per page of the PDF the receiver shows.
//!
//! LibreOffice turns `.odp` and `.pptx` into PDF without notes, so they are read from the original file: both are zip
//! archives of XML, and their notes are plain text there. Reading them directly is faster and surer than exporting
//! notes pages and pulling the text back out of a PDF. Hidden slides are left out, as LibreOffice leaves them out of
//! the PDF. A PDF has its notes in a `.pdfpc` file next to it (pdfpc, Beamer) or in its text annotations.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
};

use quick_xml::events::{BytesStart, Event};

/// The notes of each page of `pdf`, made from `original` (the same file when it is a PDF). Missing notes are "".
pub fn speaker_notes(original: &Path, pdf: &Path) -> Vec<String> {
    let ext = original
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let notes = match ext.as_str() {
        "odp" => zip_notes(original, odp_notes),
        "pptx" => zip_notes(original, pptx_notes),
        _ => None,
    };
    notes
        .or_else(|| pdfpc_notes(pdf))
        .or_else(|| std::fs::read(pdf).ok().map(|b| pdf_notes(&b)))
        .unwrap_or_default()
}

type Archive = zip::ZipArchive<std::fs::File>;

fn zip_notes(path: &Path, read: fn(&mut Archive) -> Option<Vec<String>>) -> Option<Vec<String>> {
    let mut a = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    read(&mut a)
}

/// A member of the archive as text; at most 32 MiB, so a crafted file cannot fill the memory.
fn member(a: &mut Archive, name: &str) -> Option<String> {
    let f = a.by_name(name).ok()?;
    let mut s = String::new();
    f.take(32 << 20).read_to_string(&mut s).ok()?;
    Some(s)
}

fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Lines of text joined, the blank ones at both ends cut.
fn tidy(lines: &[String]) -> String {
    lines.join("\n").trim().to_string()
}

// ---------- OpenDocument (content.xml) ----------

fn odp_notes(a: &mut Archive) -> Option<Vec<String>> {
    Some(odp_content(&member(a, "content.xml")?))
}

/// The notes of each shown slide of an ODP's content.xml.
pub fn odp_content(xml: &str) -> Vec<String> {
    let mut r = quick_xml::Reader::from_str(xml);
    let (mut hidden_styles, mut style) = (HashSet::new(), None::<String>);
    let mut slides: Vec<(bool, Vec<String>)> = Vec::new();
    // Where the reader is: in a slide's notes, in their text frame, in a paragraph.
    let (mut in_notes, mut in_frame, mut para) = (false, false, None::<String>);
    loop {
        let (e, empty) = match r.read_event() {
            Ok(Event::Start(e)) => (e, false),
            Ok(Event::Empty(e)) => (e, true),
            Ok(Event::Text(t)) => {
                if let (Some(p), true) = (para.as_mut(), in_frame) {
                    p.push_str(&t.unescape().unwrap_or_default());
                }
                continue;
            }
            Ok(Event::End(e)) => {
                match e.name().as_ref() {
                    b"style:style" => style = None,
                    b"presentation:notes" => in_notes = false,
                    b"draw:frame" => in_frame = false,
                    b"text:p" | b"text:h" => {
                        if let (Some(p), Some(s)) = (para.take(), slides.last_mut()) {
                            s.1.push(p);
                        }
                    }
                    _ => {}
                }
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => continue,
        };
        match e.name().as_ref() {
            b"style:style" if !empty => {
                style = (attr(&e, b"style:family").as_deref() == Some("drawing-page"))
                    .then(|| attr(&e, b"style:name"))
                    .flatten()
            }
            b"style:drawing-page-properties" => {
                if let (Some(s), Some("hidden")) =
                    (&style, attr(&e, b"presentation:visibility").as_deref())
                {
                    hidden_styles.insert(s.clone());
                }
            }
            b"draw:page" => {
                let hidden =
                    attr(&e, b"draw:style-name").is_some_and(|s| hidden_styles.contains(&s));
                slides.push((hidden, Vec::new()));
            }
            b"presentation:notes" if !empty => in_notes = true,
            b"draw:frame" if in_notes && !empty => {
                in_frame = attr(&e, b"presentation:class").as_deref() == Some("notes")
            }
            b"text:p" | b"text:h" if in_frame => {
                if empty {
                    if let Some(s) = slides.last_mut() {
                        s.1.push(String::new());
                    }
                } else {
                    para = Some(String::new());
                }
            }
            b"text:s" if in_frame => {
                let n = attr(&e, b"text:c")
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(1usize);
                if let Some(p) = para.as_mut() {
                    p.push_str(&" ".repeat(n.min(64)));
                }
            }
            b"text:tab" if in_frame => para.iter_mut().for_each(|p| p.push('\t')),
            b"text:line-break" if in_frame => para.iter_mut().for_each(|p| p.push('\n')),
            _ => {}
        }
    }
    slides
        .into_iter()
        .filter(|(hidden, _)| !hidden)
        .map(|(_, lines)| tidy(&lines))
        .collect()
}

// ---------- PowerPoint (ppt/...) ----------

/// `target` of a relationship, relative to the folder of the part `from` ("ppt/slides/slide1.xml").
fn resolve_part(from: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The relationships of a part: id -> (type, target part).
fn relationships(a: &mut Archive, part: &str) -> HashMap<String, (String, String)> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let Some(xml) = member(a, &format!("{dir}/_rels/{file}.rels")) else {
        return HashMap::new();
    };
    let mut r = quick_xml::Reader::from_str(&xml);
    let mut out = HashMap::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(t)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    let kind = attr(&e, b"Type").unwrap_or_default();
                    out.insert(id, (kind, resolve_part(part, &t)));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn pptx_notes(a: &mut Archive) -> Option<Vec<String>> {
    let pres = "ppt/presentation.xml";
    let xml = member(a, pres)?;
    let rels = relationships(a, pres);
    let mut order = Vec::new();
    let mut r = quick_xml::Reader::from_str(&xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.name().as_ref() == b"p:sldId" => {
                if let Some(part) = attr(&e, b"r:id").and_then(|id| rels.get(&id)) {
                    order.push(part.1.clone());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let mut notes = Vec::new();
    for slide in order {
        let Some(xml) = member(a, &slide) else {
            continue;
        };
        if pptx_hidden(&xml) {
            continue;
        }
        let text = relationships(a, &slide)
            .values()
            .find(|(kind, _)| kind.ends_with("/notesSlide"))
            .and_then(|(_, part)| member(a, part))
            .map(|x| pptx_notes_text(&x))
            .unwrap_or_default();
        notes.push(text);
    }
    Some(notes)
}

/// Whether a slide is hidden (`<p:sld show="0">`).
fn pptx_hidden(slide: &str) -> bool {
    let mut r = quick_xml::Reader::from_str(slide);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"p:sld" => {
                return matches!(attr(&e, b"show").as_deref(), Some("0" | "false"))
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

/// The text of a notes slide's body placeholder: the notes, not the slide picture or the page number.
pub fn pptx_notes_text(xml: &str) -> String {
    let mut r = quick_xml::Reader::from_str(xml);
    let (mut shape, mut body, mut in_t) = (0usize, false, false);
    let (mut lines, mut para) = (Vec::new(), String::new());
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"p:sp" => {
                    shape += 1;
                    body = false;
                }
                b"a:t" => in_t = true,
                b"a:p" => para.clear(),
                b"p:ph" => body = is_body(&e),
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"p:ph" => body = is_body(&e),
                b"a:br" if body && shape > 0 => para.push('\n'),
                _ => {}
            },
            Ok(Event::Text(t)) if in_t && body && shape > 0 => {
                para.push_str(&t.unescape().unwrap_or_default())
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"p:sp" => {
                    shape = shape.saturating_sub(1);
                    body = false;
                }
                b"a:t" => in_t = false,
                b"a:p" if body && shape > 0 => lines.push(std::mem::take(&mut para)),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    tidy(&lines)
}

fn is_body(ph: &BytesStart) -> bool {
    attr(ph, b"type").as_deref() == Some("body")
}

// ---------- PDF ----------

/// Notes from pdfpc's file next to the PDF (`talk.pdfpc` for `talk.pdf`): its JSON form (`pages[].idx`, `note`)
/// or the older text form (`[notes]`, `### <page>` headings).
fn pdfpc_notes(pdf: &Path) -> Option<Vec<String>> {
    pdfpc(&std::fs::read_to_string(pdf.with_extension("pdfpc")).ok()?)
}

pub fn pdfpc(text: &str) -> Option<Vec<String>> {
    let mut notes: Vec<String> = Vec::new();
    let mut put = |page: usize, note: &str| {
        if page < 10_000 {
            if notes.len() <= page {
                notes.resize(page + 1, String::new());
            }
            notes[page] = note.trim().to_string();
        }
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        for p in v["pages"].as_array()? {
            if let (Some(i), Some(n)) = (p["idx"].as_u64(), p["note"].as_str()) {
                put(i as usize, n);
            }
        }
        return Some(notes);
    }
    let body = text.split_once("[notes]")?.1;
    let body = body.split("\n[").next().unwrap_or(body);
    for block in body.split("### ").skip(1) {
        let (head, note) = block.split_once('\n').unwrap_or((block, ""));
        if let Ok(n) = head.trim().parse::<usize>() {
            put(n.saturating_sub(1), note);
        }
    }
    Some(notes)
}

/// The text annotations ("sticky notes") of each page, joined. A small reader of its own: objects, object streams
/// (Flate) and the page tree; enough for notes, nothing else. An encrypted PDF gives none.
pub fn pdf_notes(data: &[u8]) -> Vec<String> {
    if find(data, b"/Encrypt", 0).is_some() {
        return Vec::new();
    }
    let objs = pdf::objects(data);
    let get = |o: &pdf::Obj| -> Option<pdf::Obj> {
        match o {
            pdf::Obj::Ref(n) => objs.get(n).cloned(),
            other => Some(other.clone()),
        }
    };
    let Some(catalog) = objs
        .values()
        .find(|o| o.dict_name("Type") == Some("Catalog"))
    else {
        return Vec::new();
    };
    let mut pages = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![catalog.get("Pages").cloned().unwrap_or(pdf::Obj::Null)];
    // Depth-first, children in order: the stack holds them reversed.
    while let Some(node) = stack.pop() {
        if let pdf::Obj::Ref(n) = node {
            if !seen.insert(n) || seen.len() > 100_000 {
                continue;
            }
        }
        let Some(d) = get(&node) else { continue };
        // A node with kids is an inner node, whatever its /Type says.
        match d.get("Kids").and_then(get) {
            Some(pdf::Obj::Array(kids)) => stack.extend(kids.into_iter().rev()),
            _ if d.dict_name("Type") == Some("Page") => pages.push(d),
            _ => {}
        }
    }
    pages
        .iter()
        .map(|p| {
            let annots = match p.get("Annots").and_then(get) {
                Some(pdf::Obj::Array(a)) => a,
                _ => Vec::new(),
            };
            let texts: Vec<String> = annots
                .iter()
                .filter_map(get)
                .filter(|a| a.dict_name("Subtype") == Some("Text"))
                .filter_map(|a| match a.get("Contents") {
                    Some(pdf::Obj::Str(s)) => Some(pdf::text(s).trim().to_string()),
                    _ => None,
                })
                .filter(|t| !t.is_empty())
                .collect();
            texts.join("\n\n")
        })
        .collect()
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

mod pdf {
    use super::find;
    use std::collections::HashMap;
    use std::io::Read;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Obj {
        Null,
        Num(f64),
        Bool(bool),
        Str(Vec<u8>),
        Name(String),
        Array(Vec<Obj>),
        Dict(Vec<(String, Obj)>),
        Ref(u32),
    }

    impl Obj {
        pub fn get(&self, key: &str) -> Option<&Obj> {
            match self {
                Obj::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        pub fn dict_name(&self, key: &str) -> Option<&str> {
            match self.get(key) {
                Some(Obj::Name(n)) => Some(n),
                _ => None,
            }
        }
    }

    /// A string's text: UTF-16BE with its byte order mark, else PDFDocEncoding (taken as Latin-1).
    pub fn text(s: &[u8]) -> String {
        if let Some(rest) = s.strip_prefix(&[0xFE, 0xFF]) {
            let units: Vec<u16> = rest
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_be_bytes(c))
                .collect();
            return String::from_utf16_lossy(&units);
        }
        if let Some(rest) = s.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            return String::from_utf8_lossy(rest).into_owned();
        }
        s.iter()
            .map(|&b| b as char)
            .collect::<String>()
            .replace('\r', "\n")
    }

    /// Every `n g obj ... endobj` of the file, and the objects packed into object streams; a later one wins, as
    /// with an incremental update.
    pub fn objects(data: &[u8]) -> HashMap<u32, Obj> {
        let mut out = HashMap::new();
        let mut at = 0;
        while let Some(p) = find(data, b" obj", at) {
            at = p + 4;
            let Some(num) = number_before(data, p) else {
                continue;
            };
            let mut parser = Parser { s: data, i: p + 4 };
            let Some(obj) = parser.value(0) else { continue };
            parser.ws();
            if data[parser.i..].starts_with(b"stream") {
                let start = parser.i + 6;
                let start = start
                    + data[start..]
                        .iter()
                        .take(2)
                        .take_while(|b| matches!(b, b'\r' | b'\n'))
                        .count();
                let end = find(data, b"endstream", start).unwrap_or(data.len());
                at = end;
                if obj.dict_name("Type") == Some("ObjStm") {
                    unpack(&obj, &data[start..end], &mut out);
                }
            } else {
                at = parser.i;
            }
            out.insert(num, obj);
        }
        out
    }

    /// The object number of `n g obj` ending just before `p`.
    fn number_before(s: &[u8], p: usize) -> Option<u32> {
        let digits = |end: usize| -> Option<usize> {
            let n = s[..end]
                .iter()
                .rev()
                .take_while(|b| b.is_ascii_digit())
                .count();
            (n > 0).then_some(end - n)
        };
        let g = digits(p)?;
        let sp = g.checked_sub(1).filter(|&i| s[i] == b' ')?;
        let n = digits(sp)?;
        std::str::from_utf8(&s[n..sp]).ok()?.parse().ok()
    }

    fn unpack(dict: &Obj, raw: &[u8], out: &mut HashMap<u32, Obj>) {
        if dict.dict_name("Filter") != Some("FlateDecode") {
            return;
        }
        let mut body = Vec::new();
        if flate2::read::ZlibDecoder::new(raw)
            .take(64 << 20)
            .read_to_end(&mut body)
            .is_err()
            && body.is_empty()
        {
            return;
        }
        let num = |k| match dict.get(k) {
            Some(Obj::Num(n)) => *n as usize,
            _ => 0,
        };
        let (n, first) = (num("N"), num("First"));
        let mut head = Parser { s: &body, i: 0 };
        let mut index = Vec::new();
        for _ in 0..n.min(100_000) {
            match (head.value(0), head.value(0)) {
                (Some(Obj::Num(o)), Some(Obj::Num(off))) => index.push((o as u32, off as usize)),
                _ => break,
            }
        }
        for (o, off) in index {
            let mut p = Parser {
                s: &body,
                i: first + off,
            };
            if p.i < body.len() {
                if let Some(v) = p.value(0) {
                    out.insert(o, v);
                }
            }
        }
    }

    struct Parser<'a> {
        s: &'a [u8],
        i: usize,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<u8> {
            self.s.get(self.i).copied()
        }

        fn ws(&mut self) {
            while let Some(c) = self.peek() {
                if c == b'%' {
                    while self.peek().is_some_and(|c| c != b'\n' && c != b'\r') {
                        self.i += 1;
                    }
                } else if c.is_ascii_whitespace() || c == 0 {
                    self.i += 1;
                } else {
                    break;
                }
            }
        }

        fn value(&mut self, depth: usize) -> Option<Obj> {
            if depth > 64 {
                return None;
            }
            self.ws();
            match self.peek()? {
                b'/' => Some(Obj::Name(self.name())),
                b'(' => Some(Obj::Str(self.literal())),
                b'<' if self.s.get(self.i + 1) == Some(&b'<') => {
                    self.i += 2;
                    let mut d = Vec::new();
                    loop {
                        self.ws();
                        match self.peek()? {
                            b'>' => {
                                self.i += 2;
                                return Some(Obj::Dict(d));
                            }
                            b'/' => {
                                let k = self.name();
                                let v = self.value(depth + 1)?;
                                d.push((k, v));
                            }
                            _ => return None,
                        }
                    }
                }
                b'<' => Some(Obj::Str(self.hex())),
                b'[' => {
                    self.i += 1;
                    let mut a = Vec::new();
                    loop {
                        self.ws();
                        if self.peek()? == b']' {
                            self.i += 1;
                            return Some(Obj::Array(a));
                        }
                        a.push(self.value(depth + 1)?);
                    }
                }
                c if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' => {
                    let n = self.number()?;
                    // `n g R` is a reference.
                    let save = self.i;
                    self.ws();
                    if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        if let Some(_g) = self.number() {
                            self.ws();
                            if self.peek() == Some(b'R') {
                                self.i += 1;
                                return Some(Obj::Ref(n as u32));
                            }
                        }
                    }
                    self.i = save;
                    Some(Obj::Num(n))
                }
                _ => {
                    let w = self.word();
                    match w.as_str() {
                        "true" => Some(Obj::Bool(true)),
                        "false" => Some(Obj::Bool(false)),
                        "null" => Some(Obj::Null),
                        _ => None,
                    }
                }
            }
        }

        fn word(&mut self) -> String {
            let st = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                self.i += 1;
            }
            String::from_utf8_lossy(&self.s[st..self.i]).into_owned()
        }

        fn number(&mut self) -> Option<f64> {
            let st = self.i;
            while self
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
            {
                self.i += 1;
            }
            std::str::from_utf8(&self.s[st..self.i]).ok()?.parse().ok()
        }

        fn name(&mut self) -> String {
            self.i += 1;
            let st = self.i;
            while self
                .peek()
                .is_some_and(|c| !c.is_ascii_whitespace() && !b"/<>[]()%{}".contains(&c))
            {
                self.i += 1;
            }
            String::from_utf8_lossy(&self.s[st..self.i]).into_owned()
        }

        fn hex(&mut self) -> Vec<u8> {
            self.i += 1;
            let mut digits = Vec::new();
            while let Some(c) = self.peek() {
                self.i += 1;
                if c == b'>' {
                    break;
                }
                if let Some(d) = (c as char).to_digit(16) {
                    digits.push(d as u8);
                }
            }
            if digits.len() % 2 == 1 {
                digits.push(0);
            }
            digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect()
        }

        fn literal(&mut self) -> Vec<u8> {
            self.i += 1;
            let (mut out, mut depth) = (Vec::new(), 1);
            while let Some(c) = self.peek() {
                self.i += 1;
                match c {
                    b'(' => {
                        depth += 1;
                        out.push(c);
                    }
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        out.push(c);
                    }
                    b'\\' => {
                        let Some(e) = self.peek() else { break };
                        self.i += 1;
                        match e {
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            b'\r' => {
                                if self.peek() == Some(b'\n') {
                                    self.i += 1;
                                }
                            }
                            b'\n' => {}
                            b'0'..=b'7' => {
                                let mut v = (e - b'0') as u32;
                                for _ in 0..2 {
                                    match self.peek() {
                                        Some(d @ b'0'..=b'7') => {
                                            v = v * 8 + (d - b'0') as u32;
                                            self.i += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                out.push(v as u8);
                            }
                            other => out.push(other),
                        }
                    }
                    _ => out.push(c),
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(path: &Path, files: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("luch-notes-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    const ODP: &str = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="o" xmlns:style="s" xmlns:draw="d" xmlns:presentation="p" xmlns:text="t">
 <office:automatic-styles>
  <style:style style:name="dp1" style:family="drawing-page"><style:drawing-page-properties presentation:display-footer="true"/></style:style>
  <style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties presentation:visibility="hidden"/></style:style>
 </office:automatic-styles>
 <office:body><office:presentation>
  <draw:page draw:name="One" draw:style-name="dp1">
   <draw:frame presentation:class="title"><draw:text-box><text:p>Not a note</text:p></draw:text-box></draw:frame>
   <presentation:notes>
    <draw:page-thumbnail/>
    <draw:frame presentation:class="notes"><draw:text-box>
      <text:p>Say hello &amp; smile</text:p><text:p>Then<text:s text:c="2"/>the plan<text:line-break/>in two lines</text:p>
    </draw:text-box></draw:frame>
   </presentation:notes>
  </draw:page>
  <draw:page draw:name="Hidden" draw:style-name="dp2">
   <presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>secret</text:p></draw:text-box></draw:frame></presentation:notes>
  </draw:page>
  <draw:page draw:name="Three" draw:style-name="dp1"/>
  <draw:page draw:name="Four" draw:style-name="dp1">
   <presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Questions?</text:p></draw:text-box></draw:frame></presentation:notes>
  </draw:page>
 </office:presentation></office:body>
</office:document-content>"#;

    #[test]
    fn odp_notes_follow_the_shown_slides() {
        assert_eq!(
            odp_content(ODP),
            vec![
                "Say hello & smile\nThen  the plan\nin two lines".to_string(),
                String::new(),
                "Questions?".to_string(),
            ]
        );
        let f = temp("talk.odp");
        zip_of(
            &f,
            &[
                (
                    "mimetype",
                    "application/vnd.oasis.opendocument.presentation",
                ),
                ("content.xml", ODP),
            ],
        );
        assert_eq!(speaker_notes(&f, &f.with_extension("pdf"))[2], "Questions?");
    }

    fn notes_slide(text: &[&str]) -> String {
        let paras: String = text
            .iter()
            .map(|t| format!("<a:p><a:r><a:t>{t}</a:t></a:r></a:p>"))
            .collect();
        format!(
            r#"<p:notes xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr>
<p:txBody>{paras}</p:txBody></p:sp>
<p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum" idx="5"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>7</a:t></a:r></a:p></p:txBody></p:sp>
</p:spTree></p:cSld></p:notes>"#
        )
    }

    #[test]
    fn pptx_notes_in_slide_order_without_hidden_slides() {
        let rel = |id: &str, kind: &str, target: &str| {
            format!(
                r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
            )
        };
        let rels = |body: String| format!(r#"<Relationships xmlns="r">{body}</Relationships>"#);
        let pres = r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst>
<p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId4"/></p:sldIdLst></p:presentation>"#;
        let pres_rels = rels(
            rel("rId2", "slide", "slides/slide1.xml")
                + &rel("rId3", "slide", "slides/slide2.xml")
                + &rel("rId4", "slide", "/ppt/slides/slide3.xml"),
        );
        let slide = |show: &str| format!(r#"<p:sld xmlns:p="p"{show}><p:cSld/></p:sld>"#);
        let slide_rels = |n: u32| {
            rels(
                rel("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml")
                    + &rel(
                        "rId2",
                        "notesSlide",
                        &format!("../notesSlides/notesSlide{n}.xml"),
                    ),
            )
        };
        let f = temp("deck.pptx");
        let (n1, n2) = (
            notes_slide(&["First &lt;b&gt;", "line two"]),
            notes_slide(&["Opening"]),
        );
        zip_of(
            &f,
            &[
                ("ppt/presentation.xml", pres),
                ("ppt/_rels/presentation.xml.rels", &pres_rels),
                ("ppt/slides/slide1.xml", &slide("")),
                ("ppt/slides/slide2.xml", &slide("")),
                ("ppt/slides/slide3.xml", &slide(r#" show="0""#)),
                ("ppt/slides/_rels/slide1.xml.rels", &slide_rels(1)),
                ("ppt/slides/_rels/slide2.xml.rels", &slide_rels(2)),
                ("ppt/slides/_rels/slide3.xml.rels", &slide_rels(1)),
                ("ppt/notesSlides/notesSlide1.xml", &n1),
                ("ppt/notesSlides/notesSlide2.xml", &n2),
            ],
        );
        // The order of sldIdLst, not of the file names; the page number placeholder is not a note.
        assert_eq!(
            speaker_notes(&f, &f.with_extension("pdf")),
            vec!["Opening", "First <b>\nline two"]
        );
    }

    #[test]
    fn pdfpc_files_in_both_forms() {
        let json = r#"{"pdfpcFormat":2,"pages":[{"idx":0,"label":"1","note":"Hi"},{"idx":2,"label":"2","note":" Demo "}]}"#;
        assert_eq!(pdfpc(json).unwrap(), vec!["Hi", "", "Demo"]);
        let old = "[file]\ntalk.pdf\n[notes]\n### 1\nfirst\nmore\n### 3\nthird\n[end]\n";
        assert_eq!(pdfpc(old).unwrap(), vec!["first\nmore", "", "third"]);
        let pdf = temp("beamer.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        std::fs::write(pdf.with_extension("pdfpc"), json).unwrap();
        assert_eq!(speaker_notes(&pdf, &pdf)[2], "Demo");
    }

    /// A PDF written by hand: two pages, a sticky note on the second (in UTF-16), a link that is no note.
    fn plain_pdf() -> Vec<u8> {
        let mut s = b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n".to_vec();
        s.extend_from_slice(b"2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n");
        s.extend_from_slice(b"3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >> endobj\n");
        s.extend_from_slice(b"4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots 7 0 R >> endobj\n");
        s.extend_from_slice(
            b"5 0 obj << /Type /Annot /Subtype /Link /Contents (not a note) >> endobj\n",
        );
        s.extend_from_slice(b"6 0 obj << /Type /Annot /Subtype /Text /Contents <FEFF0041006E0064002004110435> >> endobj\n");
        s.extend_from_slice(b"7 0 obj [6 0 R 8 0 R] endobj\n");
        s.extend_from_slice(
            b"8 0 obj << /Subtype /Text /Contents (Second \\(note\\)\\nline\\101) >> endobj\n",
        );
        s.extend_from_slice(b"trailer << /Root 1 0 R >>\n%%EOF\n");
        s
    }

    #[test]
    fn pdf_text_annotations_are_notes() {
        assert_eq!(
            pdf_notes(&plain_pdf()),
            vec!["".to_string(), "And Бе\n\nSecond (note)\nlineA".to_string()]
        );
        let encrypted = [plain_pdf(), b"trailer << /Encrypt 9 0 R >>".to_vec()].concat();
        assert!(pdf_notes(&encrypted).is_empty());
    }

    #[test]
    fn pdf_notes_inside_compressed_object_streams() {
        // Pages and annotations packed into an object stream, as PDF 1.5 writers do.
        let packed = b"<< /Type /Pages /Kids [11 0 R] /Count 1 >> << /Type /Page /MediaBox [0 0 1 1] /Annots [12 0 R] >> << /Subtype /Text /Contents (Packed) >>";
        let (a, b) = (packed.iter().position(|&c| c == b'>').unwrap() + 2, 0);
        let second = packed[a..].windows(2).position(|w| w == b"<<").unwrap() + a;
        let third = packed[second + 2..]
            .windows(2)
            .position(|w| w == b"<<")
            .unwrap()
            + second
            + 2;
        let head = format!("10 {b} 11 {second} 12 {third} ");
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(head.as_bytes()).unwrap();
        z.write_all(packed).unwrap();
        let body = z.finish().unwrap();
        let mut pdf = b"%PDF-1.5\n1 0 obj << /Type /Catalog /Pages 10 0 R >> endobj\n".to_vec();
        pdf.extend_from_slice(
            format!("20 0 obj << /Type /ObjStm /N 3 /First {} /Filter /FlateDecode /Length {} >>\nstream\n", head.len(), body.len())
                .as_bytes(),
        );
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        assert_eq!(pdf_notes(&pdf), vec!["Packed"]);
    }

    #[test]
    fn parts_resolve_relative_to_their_folder() {
        assert_eq!(
            resolve_part("ppt/slides/slide1.xml", "../notesSlides/n1.xml"),
            "ppt/notesSlides/n1.xml"
        );
        assert_eq!(
            resolve_part("ppt/presentation.xml", "slides/slide1.xml"),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve_part("ppt/presentation.xml", "/ppt/slides/s.xml"),
            "ppt/slides/s.xml"
        );
    }
}
