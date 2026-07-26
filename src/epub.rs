extern crate alloc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// ── Public error type ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum EpubError {
    /// The data does not look like a ZIP archive.
    NotZip,
    /// Data ended before we could finish reading a structure.
    Truncated,
    /// A ZIP signature field contained an unexpected value.
    BadSignature,
    /// A filename or XML file was not valid UTF-8.
    Utf8Error,
    /// A required ZIP entry was not found by name.
    EntryNotFound,
    /// `META-INF/container.xml` did not contain a `full-path` attribute.
    MissingOpfPath,
    /// The OPF spine was empty or could not be resolved against the manifest.
    MissingSpine,
    /// DEFLATE decompression failed.
    DecompressFailed,
    /// ZIP compression method is not 0 (stored) or 8 (deflated).
    UnsupportedMethod(u16),
}

// ── ZIP internals ─────────────────────────────────────────────────────────────

struct ZipEntry {
    name:         String,
    local_offset: u32,  // byte offset of local file header in `data`
    comp_size:    u32,
    method:       u16,  // 0 = stored, 8 = deflated
}

#[inline]
fn u16le(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

#[inline]
fn u32le(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

/// Scan `data` for the ZIP End-of-Central-Directory record, then walk the
/// central directory and return one [`ZipEntry`] per file.
fn parse_cdr(data: &[u8]) -> Result<Vec<ZipEntry>, EpubError> {
    let n = data.len();
    if n < 22 { return Err(EpubError::Truncated); }

    // Locate EOCD by scanning backwards (handles up to 64 KiB of ZIP comment).
    const EOCD_SIG: u32 = 0x0605_4b50;
    let scan_from = n.saturating_sub(22 + 65535);
    let eocd = (scan_from..=n - 22)
        .rev()
        .find(|&i| u32le(data, i) == EOCD_SIG)
        .ok_or(EpubError::NotZip)?;

    let cd_count  = u16le(data, eocd + 10) as usize;
    let cd_offset = u32le(data, eocd + 16) as usize;

    let mut entries = Vec::with_capacity(cd_count);
    let mut pos = cd_offset;

    for _ in 0..cd_count {
        if pos + 46 > n { return Err(EpubError::Truncated); }
        if u32le(data, pos) != 0x0201_4b50 { return Err(EpubError::BadSignature); }

        let method       = u16le(data, pos + 10);
        let comp_size    = u32le(data, pos + 20);
        let fname_len    = u16le(data, pos + 28) as usize;
        let extra_len    = u16le(data, pos + 30) as usize;
        let comment_len  = u16le(data, pos + 32) as usize;
        let local_offset = u32le(data, pos + 42);

        let name_start = pos + 46;
        let name_end   = name_start + fname_len;
        if name_end > n { return Err(EpubError::Truncated); }

        let name = core::str::from_utf8(&data[name_start..name_end])
            .map_err(|_| EpubError::Utf8Error)?
            .to_string();

        entries.push(ZipEntry { name, local_offset, comp_size, method });
        pos = name_end + extra_len + comment_len;
    }

    Ok(entries)
}

/// Decompress (or copy) the bytes of a single ZIP entry from `data`.
fn extract_entry(data: &[u8], entry: &ZipEntry) -> Result<Vec<u8>, EpubError> {
    let s = entry.local_offset as usize;
    if s + 30 > data.len() { return Err(EpubError::Truncated); }
    if u32le(data, s) != 0x0403_4b50 { return Err(EpubError::BadSignature); }

    let fname_len  = u16le(data, s + 26) as usize;
    let extra_len  = u16le(data, s + 28) as usize;
    let data_start = s + 30 + fname_len + extra_len;
    let data_end   = data_start + entry.comp_size as usize;

    if data_end > data.len() { return Err(EpubError::Truncated); }
    let compressed = &data[data_start..data_end];

    match entry.method {
        0 => Ok(compressed.to_vec()),
        8 => miniz_oxide::inflate::decompress_to_vec(compressed)
                .map_err(|_| EpubError::DecompressFailed),
        m => Err(EpubError::UnsupportedMethod(m)),
    }
}

// ── Public EPUB archive ───────────────────────────────────────────────────────

/// An EPUB archive backed by a byte slice (e.g. from `include_bytes!` or an
/// SD-card buffer). The ZIP central directory is parsed once at construction;
/// individual chapters are decompressed on demand.
pub struct EpubArchive<'a> {
    data:    &'a [u8],
    entries: Vec<ZipEntry>,
}

impl<'a> EpubArchive<'a> {
    /// Parse the ZIP central directory. Cheap — does not decompress anything.
    pub fn new(data: &'a [u8]) -> Result<Self, EpubError> {
        let entries = parse_cdr(data)?;
        Ok(Self { data, entries })
    }

    /// Return the ordered list of chapter XHTML paths (relative to the ZIP root)
    /// by parsing `META-INF/container.xml` and the OPF file.
    pub fn spine(&self) -> Result<Vec<String>, EpubError> {
        // Step 1: container.xml → OPF path
        let container = self.extract_named("META-INF/container.xml")?;
        let container_str = core::str::from_utf8(&container).map_err(|_| EpubError::Utf8Error)?;
        let opf_path = parse_container(container_str)?;

        // Step 2: OPF directory prefix (for resolving relative hrefs)
        let opf_dir: &str = match opf_path.rfind('/') {
            Some(i) => &opf_path[..=i],   // includes trailing '/'
            None    => "",
        };

        // Step 3: OPF → spine
        let opf_bytes = self.extract_named(&opf_path)?;
        let opf_str = core::str::from_utf8(&opf_bytes).map_err(|_| EpubError::Utf8Error)?;
        parse_opf(opf_str, opf_dir)
    }

    /// Decompress the XHTML file at `path` (a value from [`spine`]) and return
    /// its content as plain text with paragraph breaks preserved.
    pub fn chapter_text(&self, path: &str) -> Result<String, EpubError> {
        let bytes = self.extract_named(path)?;
        let xhtml = core::str::from_utf8(&bytes).map_err(|_| EpubError::Utf8Error)?;
        Ok(strip_xhtml(xhtml))
    }

    fn extract_named(&self, name: &str) -> Result<Vec<u8>, EpubError> {
        let entry = self.find_entry(name).ok_or(EpubError::EntryNotFound)?;
        extract_entry(self.data, entry)
    }

    fn find_entry(&self, name: &str) -> Option<&ZipEntry> {
        // 1. Exact match.
        if let Some(e) = self.entries.iter().find(|e| e.name == name) {
            return Some(e);
        }
        // 2. Strip leading "./" from stored names (some generators add it).
        let want = name.trim_start_matches("./");
        self.entries.iter().find(|e| e.name.trim_start_matches("./") == want)
    }
}

// ── XML parsing (container.xml and content.opf) ───────────────────────────────

/// Extract the OPF path from `META-INF/container.xml`.
fn parse_container(xml: &str) -> Result<String, EpubError> {
    use xmlparser::Token;
    let mut in_rootfile = false;
    for token in xmlparser::Tokenizer::from(xml) {
        let token = match token { Ok(t) => t, Err(_) => continue };
        match token {
            Token::ElementStart { local, .. } => {
                in_rootfile = local == "rootfile";
            }
            Token::Attribute { local, value, .. } if in_rootfile && local == "full-path" => {
                return Ok(value.as_str().to_string());
            }
            _ => {}
        }
    }
    Err(EpubError::MissingOpfPath)
}

/// Parse the OPF file and return the ordered list of chapter XHTML paths.
/// `opf_dir` is the directory prefix of the OPF file itself (e.g. `"OEBPS/"`),
/// used to resolve manifest hrefs that are relative to the OPF location.
fn parse_opf(opf: &str, opf_dir: &str) -> Result<Vec<String>, EpubError> {
    use xmlparser::{ElementEnd, Token};

    // Pass 1: collect manifest entries (id → href pairs).
    let mut manifest: Vec<(String, String)> = Vec::new();
    let mut spine_idrefs: Vec<String> = Vec::new();

    let mut in_manifest = false;
    let mut in_spine    = false;

    // Temporary buffers for the current element's attributes.
    let mut cur_id    = String::new();
    let mut cur_href  = String::new();
    let mut cur_idref = String::new();
    let mut in_item     = false;
    let mut in_itemref  = false;

    for token in xmlparser::Tokenizer::from(opf) {
        let token = match token { Ok(t) => t, Err(_) => continue };

        match token {
            Token::ElementStart { local, .. } => {
                match local.as_str() {
                    "manifest" => { in_manifest = true; in_spine = false; }
                    "spine"    => { in_spine = true;    in_manifest = false; }
                    "item" if in_manifest => {
                        in_item = true;
                        cur_id.clear();
                        cur_href.clear();
                    }
                    "itemref" if in_spine => {
                        in_itemref = true;
                        cur_idref.clear();
                    }
                    _ => {}
                }
            }

            Token::Attribute { local, value, .. } => {
                if in_item {
                    match local.as_str() {
                        "id"   => { cur_id.clear();   cur_id.push_str(value.as_str()); }
                        "href" => { cur_href.clear();  cur_href.push_str(value.as_str()); }
                        _ => {}
                    }
                } else if in_itemref && local == "idref" {
                    cur_idref.clear();
                    cur_idref.push_str(value.as_str());
                }
            }

            Token::ElementEnd { end, .. } => {
                match end {
                    // Self-closing tag: commit the buffered entry.
                    ElementEnd::Empty => {
                        if in_item && !cur_id.is_empty() && !cur_href.is_empty() {
                            manifest.push((cur_id.clone(), cur_href.clone()));
                        }
                        if in_itemref && !cur_idref.is_empty() {
                            spine_idrefs.push(cur_idref.clone());
                        }
                        in_item    = false;
                        in_itemref = false;
                    }
                    // Closing tag: handle block-level closes and non-empty elements.
                    ElementEnd::Close(_, local) => {
                        match local.as_str() {
                            "manifest" => { in_manifest = false; }
                            "spine"    => { in_spine    = false; }
                            "item" if in_item => {
                                if !cur_id.is_empty() && !cur_href.is_empty() {
                                    manifest.push((cur_id.clone(), cur_href.clone()));
                                }
                                in_item = false;
                            }
                            "itemref" if in_itemref => {
                                if !cur_idref.is_empty() {
                                    spine_idrefs.push(cur_idref.clone());
                                }
                                in_itemref = false;
                            }
                            _ => {}
                        }
                    }
                    ElementEnd::Open => {} // opening `>` — attributes are done, element not closed
                }
            }

            _ => {}
        }
    }

    if spine_idrefs.is_empty() { return Err(EpubError::MissingSpine); }

    // Resolve spine idrefs → full ZIP paths.
    let mut result = Vec::with_capacity(spine_idrefs.len());
    for idref in &spine_idrefs {
        if let Some((_, href)) = manifest.iter().find(|(id, _)| id == idref) {
            let mut path = String::with_capacity(opf_dir.len() + href.len());
            path.push_str(opf_dir);
            path.push_str(href);
            result.push(path);
        }
    }

    if result.is_empty() { Err(EpubError::MissingSpine) } else { Ok(result) }
}

// ── XHTML → plain text ────────────────────────────────────────────────────────

/// Strip HTML tags from EPUB chapter XHTML, returning plain text with
/// paragraph breaks (`\n\n`) and forced line breaks (`\n`) preserved.
///
/// Uses a byte-level scanner rather than `xmlparser` so that common XHTML
/// quirks (e.g. `&nbsp;` without a DTD, `<br>` without the self-close slash)
/// are handled gracefully instead of causing parse errors.
fn strip_xhtml(xhtml: &str) -> String {
    let mut out = String::with_capacity(xhtml.len() / 2);
    let bytes = xhtml.as_bytes();
    let n = bytes.len();
    let mut pos = 0;

    // State
    let mut in_body    = false;
    let mut skip_depth = 0i32;   // >0 while inside <head>, <script>, <style>
    let mut in_tag     = false;
    let mut tag_buf    = [0u8; 32]; // lowercase tag name accumulator
    let mut tag_len    = 0usize;
    let mut tag_close  = false;     // true if we saw '</'
    let mut past_name  = false;     // true once tag name is complete
    let mut text_start = 0usize;    // start of the current plain-text run

    macro_rules! flush_text {
        ($end:expr) => {
            if in_body && skip_depth == 0 && text_start < $end {
                let slice = &xhtml[text_start..$end];
                // Normalize XML whitespace: runs of whitespace → single space,
                // but don't insert a space when out already ends with a newline.
                let mut prev_ws = matches!(out.as_bytes().last(), Some(&b'\n') | Some(&b' ') | None);
                for ch in slice.chars() {
                    if ch.is_ascii_whitespace() {
                        if !prev_ws {
                            out.push(' ');
                            prev_ws = true;
                        }
                    } else {
                        out.push(ch);
                        prev_ws = false;
                    }
                }
            }
        };
    }

    while pos < n {
        let b = bytes[pos];

        if in_tag {
            match b {
                b'>' => {
                    in_tag = false;
                    apply_tag(&tag_buf[..tag_len], tag_close, &mut out, &mut in_body, &mut skip_depth);
                    tag_len = 0; tag_close = false; past_name = false;
                    text_start = pos + 1;
                    pos += 1;
                }
                b'/' if !past_name && pos + 1 < n && bytes[pos + 1] == b'>' => {
                    // Self-closing: treat same as closing tag for block elements.
                    in_tag = false;
                    apply_tag(&tag_buf[..tag_len], true, &mut out, &mut in_body, &mut skip_depth);
                    tag_len = 0; tag_close = false; past_name = false;
                    text_start = pos + 2;
                    pos += 2;
                }
                _ if past_name => { pos += 1; } // inside attribute content — skip
                b' ' | b'\t' | b'\n' | b'\r' | b'/' => {
                    past_name = true;
                    pos += 1;
                }
                _ => {
                    if tag_len < 31 {
                        tag_buf[tag_len] = b.to_ascii_lowercase();
                        tag_len += 1;
                    }
                    pos += 1;
                }
            }
        } else {
            match b {
                b'<' => {
                    flush_text!(pos);
                    in_tag = true; tag_len = 0; tag_close = false; past_name = false;
                    pos += 1;

                    // Handle </tag>, <!-- comments -->, <!DOCTYPE>, <?PI?>
                    if pos < n {
                        match bytes[pos] {
                            b'/' => { tag_close = true; pos += 1; }
                            b'!' => {
                                if pos + 2 < n && bytes[pos+1] == b'-' && bytes[pos+2] == b'-' {
                                    // HTML comment: find -->
                                    pos += 3;
                                    while pos + 2 < n {
                                        if bytes[pos]==b'-' && bytes[pos+1]==b'-' && bytes[pos+2]==b'>' {
                                            pos += 3; break;
                                        }
                                        pos += 1;
                                    }
                                } else {
                                    // DOCTYPE or CDATA
                                    while pos < n && bytes[pos] != b'>' { pos += 1; }
                                    if pos < n { pos += 1; }
                                }
                                in_tag = false;
                                text_start = pos;
                            }
                            b'?' => {
                                // Processing instruction
                                while pos < n && bytes[pos] != b'>' { pos += 1; }
                                if pos < n { pos += 1; }
                                in_tag = false;
                                text_start = pos;
                            }
                            _ => {}
                        }
                    }
                }

                b'&' => {
                    flush_text!(pos);
                    pos += 1;
                    let start = pos;
                    // Scan to ';', but cap at 16 chars to avoid runaway on malformed input.
                    while pos < n && bytes[pos] != b';' && pos - start < 16 { pos += 1; }
                    if pos < n {
                        let entity = &xhtml[start..pos];
                        pos += 1; // consume ';'
                        if in_body && skip_depth == 0 {
                            let decoded: &str = match entity {
                                "amp"  => "&",
                                "lt"   => "<",
                                "gt"   => ">",
                                "quot" => "\"",
                                "apos" => "'",
                                "nbsp" => " ",
                                s if s.starts_with('#') => {
                                    let num = &s[1..];
                                    let code: Option<u32> =
                                        if num.starts_with('x') || num.starts_with('X') {
                                            u32::from_str_radix(&num[1..], 16).ok()
                                        } else {
                                            num.parse().ok()
                                        };
                                    if let Some(c) = code.and_then(char::from_u32) {
                                        out.push(c);
                                    }
                                    ""
                                }
                                _ => " ", // unknown entity → space
                            };
                            out.push_str(decoded);
                        }
                    }
                    text_start = pos;
                }

                _ => { pos += 1; } // accumulate into text run
            }
        }
    }

    flush_text!(n);

    normalize_breaks(out)
}

/// Emit the effect of an HTML tag on the output string and body/skip state.
fn apply_tag(
    tag:        &[u8],
    closing:    bool,
    out:        &mut String,
    in_body:    &mut bool,
    skip_depth: &mut i32,
) {
    let name = core::str::from_utf8(tag).unwrap_or("");

    match name {
        "body" if !closing => { *in_body = true; }

        "head" | "script" | "style" => {
            if closing {
                if *skip_depth > 0 { *skip_depth -= 1; }
                // After </head>, body begins (handles documents with no explicit <body>).
                if name == "head" && !*in_body { *in_body = true; }
            } else {
                *skip_depth += 1;
            }
        }

        _ if !*in_body || *skip_depth > 0 => { /* not in renderable content */ }

        "br" => {
            // Trim trailing space then insert a line break.
            while out.ends_with(' ') { out.pop(); }
            out.push('\n');
        }

        "p" | "div" | "blockquote" if closing => push_para_break(out),

        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            // Heading open or close: insert a paragraph-style break on both sides.
            push_para_break(out);
        }

        "li" if !closing => {
            while out.ends_with(' ') { out.pop(); }
            if !out.ends_with('\n') { out.push('\n'); }
        }

        _ => {}
    }
}

/// Ensure the output ends with exactly two newlines (paragraph break).
fn push_para_break(out: &mut String) {
    while out.ends_with(' ') { out.pop(); }
    if out.ends_with("\n\n") {
        // already a paragraph break
    } else if out.ends_with('\n') {
        out.push('\n');
    } else if !out.is_empty() {
        out.push_str("\n\n");
    }
}

/// Collapse runs of more than two consecutive newlines to exactly two.
fn normalize_breaks(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut nl = 0u8;
    for ch in s.chars() {
        if ch == '\n' {
            nl += 1;
            if nl <= 2 { out.push('\n'); }
        } else {
            nl = 0;
            out.push(ch);
        }
    }
    out
}
