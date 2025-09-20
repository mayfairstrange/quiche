// range.rs
//
// Public interface for parsing + handling HTTP Range (RFC 7233) in the
// quiche example server. Implementation is TODO.
//
// This module is focused on the "bytes" unit and supports:
// - Single range  -> 206 Partial Content with Content-Range
// - Multiple ranges -> 206 + multipart/byteranges with boundary
// - Unsatisfiable  -> 416 Range Not Satisfiable + Content-Range: bytes */<len>
// - If-Range gating (ETag or Last-Modified) -> full 200 when validator mismatches
//
// The main entry point is `handle_range_request(...)` which returns either:
//   - Ok(Some(RangeResponse))   => replace your normal 200 response
//   - Ok(None)                  => no Range handling needed (send your normal 200)
//   - Err(e)                    => malformed/invalid Range (treat as 416 or fallback)

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use quiche::h3::NameValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeUnit {
    Bytes,
    // Future: other units (not used today)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSpec {
    /// Inclusive start (None means suffix range, e.g., "-500")
    pub start: Option<u64>,
    /// Inclusive end (None means open range to EOF, e.g., "500-")
    pub end: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RangeConfig {
    /// Allow multipart/byteranges when multiple ranges are requested.
    pub enable_multipart: bool,
    /// Hard cap on number of accepted ranges (avoid abuse).
    pub max_parts: usize,
}

impl Default for RangeConfig {
    fn default() -> Self {
        Self {
            enable_multipart: true,
            max_parts: 16,
        }
    }
}

/// If-Range validator per RFC 7233 §3.2.
/// Exactly one of `etag` or `last_modified_http_date` should be Some(..)
#[derive(Debug, Clone)]
pub struct IfRange {
    pub etag: Option<String>,
    /// An HTTP-date string (already formatted per RFC 7231), not SystemTime.
    pub last_modified_http_date: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RangeContext {
    /// File path that will back the response body (used to read slices).
    pub path: PathBuf,
    /// Total size of the full representation in bytes.
    pub total_len: u64,
    /// ETag (strong or weak) of the full representation, if known.
    pub etag: Option<String>,
    /// Last-Modified as HTTP-date text (e.g., "Wed, 21 Oct 2015 07:28:00 GMT").
    pub last_modified_http_date: Option<String>,
    /// Content-Type for the full representation (used for single-range
    /// and as part type for multipart/byteranges).
    pub content_type: Option<Vec<u8>>,
    /// If-Range validator parsed upstream (optional).
    pub if_range: Option<IfRange>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum RangeError {
    /// "Range" header exists but is syntactically invalid (bad unit, bad byte-range-set)
    Parse,
    /// Unit not supported (e.g., "items=.."). Only "bytes" is supported.
    UnsupportedUnit,
    /// Too many ranges (exceeds RangeConfig::max_parts).
    TooManyParts,
    /// The requested range-set is unsatisfiable for this representation.
    Unsatisfiable,
    /// I/O errors while building the body (reading file slices).
    Io(std::io::Error),
}

impl From<std::io::Error> for RangeError {
    fn from(e: std::io::Error) -> Self {
        RangeError::Io(e)
    }
}

#[derive(Debug)]
pub struct RangeResponse {
    pub status: u16,                      // 206 or 416
    pub headers: Vec<quiche::h3::Header>, // full header set for this response
    pub body: Vec<u8>,                    // complete entity for this response
}

/// Optional, mostly for testing/inspection if you need it later.
#[derive(Debug, Clone)]
pub struct ParsedRangeHeader {
    pub unit: RangeUnit,       // currently always Bytes
    pub specs: Vec<RangeSpec>, // normalized as given, before evaluation
}

fn header_value<'a>(
    headers: &'a [quiche::h3::Header], name_lc: &[u8],
) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|h| h.name().eq_ignore_ascii_case(name_lc))
        .map(|h| h.value())
}

fn parse_bytes_unit(s: &str) -> Result<(), RangeError> {
    // Accept exactly "bytes"
    if s.trim().eq_ignore_ascii_case("bytes") {
        Ok(())
    } else {
        Err(RangeError::UnsupportedUnit)
    }
}

fn parse_range_specs(s: &str) -> Result<Vec<RangeSpec>, RangeError> {
    // s is the byte-range-set (after "bytes="), e.g. "0-499,500-,-500,9500-"
    let mut out = Vec::new();
    for part in s.split(',') {
        let p = part.trim();
        if p.is_empty() {
            return Err(RangeError::Parse);
        }
        let mut it = p.splitn(2, '-');
        let a = it.next().unwrap().trim();
        let b = it.next().ok_or(RangeError::Parse)?.trim();

        let spec = if a.is_empty() {
            // "-suffix"
            let suf: u64 = b.parse().map_err(|_| RangeError::Parse)?;
            RangeSpec {
                start: None,
                end: Some(suf),
            } // end used as suffix length placeholder
        } else if b.is_empty() {
            // "start-"
            let start: u64 = a.parse().map_err(|_| RangeError::Parse)?;
            RangeSpec {
                start: Some(start),
                end: None,
            }
        } else {
            // "start-end"
            let start: u64 = a.parse().map_err(|_| RangeError::Parse)?;
            let end: u64 = b.parse().map_err(|_| RangeError::Parse)?;
            if end < start {
                return Err(RangeError::Parse);
            }
            RangeSpec {
                start: Some(start),
                end: Some(end),
            }
        };

        out.push(spec);
    }

    Ok(out)
}

fn evaluate_specs(
    specs: &[RangeSpec], total_len: u64,
) -> Result<Vec<(u64, u64)>, RangeError> {
    if total_len == 0 {
        // RFC: for empty representation, any byte-range is unsatisfiable
        return Err(RangeError::Unsatisfiable);
    }

    let mut out = Vec::with_capacity(specs.len());
    for s in specs {
        let (start, end) = match (s.start, s.end) {
            (Some(st), Some(en)) => (st, en),
            (Some(st), None) => {
                if st >= total_len {
                    continue; // invalid -> drop
                }
                (st, total_len - 1)
            },
            (None, Some(suffix_len)) => {
                if suffix_len == 0 {
                    continue; // "-0" is invalid
                }
                if suffix_len >= total_len {
                    (0, total_len - 1)
                } else {
                    (total_len - suffix_len, total_len - 1)
                }
            },
            (None, None) => continue, // impossible
        };

        if start >= total_len {
            continue;
        }

        let end = end.min(total_len - 1);
        if end < start {
            continue;
        }

        out.push((start, end));
    }

    if out.is_empty() {
        Err(RangeError::Unsatisfiable)
    } else {
        Ok(out)
    }
}

fn read_slice(
    path: &PathBuf, start: u64, end: u64,
) -> Result<Vec<u8>, RangeError> {
    let mut f = File::open(path)?;
    let len = end - start + 1;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

fn gen_boundary() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("quiche_boundary_{nanos:x}")
}

pub fn handle_range_request(
    request_headers: &[quiche::h3::Header], ctx: &RangeContext, cfg: &RangeConfig,
) -> Result<Option<RangeResponse>, RangeError> {
    let range_raw = match header_value(request_headers, b"range") {
        Some(v) => std::str::from_utf8(v).map_err(|_| RangeError::Parse)?,
        None => return Ok(None), // no range -> normal 200 by caller
    };

    if let Some(ir) = &ctx.if_range {
        // If entity-tag present, must match exactly (strong compare is ideal; simplified here)
        if let Some(ir_etag) = &ir.etag {
            if let Some(cur_etag) = &ctx.etag {
                if ir_etag != cur_etag {
                    return Ok(None); // validator mismatch => send full 200 by caller
                }
            } else {
                return Ok(None); // we can't validate -> be safe & send full 200
            }
        }
        // If HTTP-date present, only honor if dates match (simplified: strict equality)
        if let Some(ir_date) = &ir.last_modified_http_date {
            if let Some(cur_date) = &ctx.last_modified_http_date {
                if ir_date != cur_date {
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }
    }

    let mut parts = range_raw.splitn(2, '=');
    let unit = parts.next().ok_or(RangeError::Parse)?.trim();
    let set = parts.next().ok_or(RangeError::Parse)?;

    parse_bytes_unit(unit)?;
    let specs = parse_range_specs(set)?;
    if specs.is_empty() {
        return Err(RangeError::Parse);
    }

    let trimmed_specs: Vec<RangeSpec> =
        specs.into_iter().take(cfg.max_parts).collect();

    let ranges = evaluate_specs(&trimmed_specs, ctx.total_len);

    let ranges = match ranges {
        Ok(r) => r,
        Err(RangeError::Unsatisfiable) => {
            // 416 with Content-Range: bytes */len
            let headers = vec![
                quiche::h3::Header::new(b":status", b"416"),
                quiche::h3::Header::new(b"server", b"quiche"),
                quiche::h3::Header::new(
                    b"content-range",
                    format!("bytes */{}", ctx.total_len).as_bytes(),
                ),
                quiche::h3::Header::new(b"content-length", b"0"),
            ];
            return Ok(Some(RangeResponse {
                status: 416,
                headers,
                body: Vec::new(),
            }));
        },
        Err(e) => return Err(e),
    };

    if ranges.len() == 1 || !cfg.enable_multipart {
        // Single part (or forced single by config): 206 + Content-Range + body slice
        let (start, end) = ranges[0];
        let body = read_slice(&ctx.path, start, end)?;

        let mut headers = Vec::with_capacity(5);
        headers.push(quiche::h3::Header::new(b":status", b"206"));
        headers.push(quiche::h3::Header::new(b"server", b"quiche"));
        headers.push(quiche::h3::Header::new(
            b"content-range",
            format!("bytes {}-{}/{}", start, end, ctx.total_len).as_bytes(),
        ));
        headers.push(quiche::h3::Header::new(
            b"content-length",
            body.len().to_string().as_bytes(),
        ));
        if let Some(ct) = &ctx.content_type {
            headers.push(quiche::h3::Header::new(b"content-type", ct.as_slice()));
        }

        return Ok(Some(RangeResponse {
            status: 206,
            headers,
            body,
        }));
    }

    let boundary = gen_boundary();
    let boundary_open = format!("--{}\r\n", boundary).into_bytes();
    let boundary_close = format!("--{}--\r\n", boundary).into_bytes();

    let part_ct = ctx
        .content_type
        .clone()
        .unwrap_or_else(|| b"application/octet-stream".to_vec());

    // Assemble body
    let mut body = Vec::new();
    for (start, end) in &ranges {
        body.extend_from_slice(&boundary_open);

        // Per-part headers
        body.extend_from_slice(b"Content-Type: ");
        body.extend_from_slice(&part_ct);
        body.extend_from_slice(b"\r\n");

        body.extend_from_slice(b"Content-Range: ");
        body.extend_from_slice(
            format!("bytes {}-{}/{}", start, end, ctx.total_len).as_bytes(),
        );
        body.extend_from_slice(b"\r\n\r\n");

        // Per-part payload
        let slice = read_slice(&ctx.path, *start, *end)?;
        body.extend_from_slice(&slice);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(&boundary_close);

    let headers = vec![
        quiche::h3::Header::new(b":status", b"206"),
        quiche::h3::Header::new(b"server", b"quiche"),
        quiche::h3::Header::new(
            b"content-type",
            format!("multipart/byteranges; boundary={}", boundary).as_bytes(),
        ),
        quiche::h3::Header::new(
            b"content-length",
            body.len().to_string().as_bytes(),
        ),
    ];

    Ok(Some(RangeResponse {
        status: 206,
        headers,
        body,
    }))
}

pub fn parse_range_header_only(
    request_headers: &[quiche::h3::Header],
) -> Result<Option<ParsedRangeHeader>, RangeError> {
    let range_raw = match header_value(request_headers, b"range") {
        Some(v) => std::str::from_utf8(v).map_err(|_| RangeError::Parse)?,
        None => return Ok(None),
    };

    let mut parts = range_raw.splitn(2, '=');
    let unit = parts.next().ok_or(RangeError::Parse)?.trim();
    let set = parts.next().ok_or(RangeError::Parse)?;

    parse_bytes_unit(unit)?;
    let specs = parse_range_specs(set)?;

    Ok(Some(ParsedRangeHeader {
        unit: RangeUnit::Bytes,
        specs,
    }))
}



#[cfg(test)]
mod tests {
    use super::*;
    use quiche::h3::Header;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn hdr(name: &[u8], value: &[u8]) -> Header {
        Header::new(name, value)
    }

    fn get_header<'a>(headers: &'a [Header], name: &str) -> Option<&'a [u8]> {
        headers.iter()
            .find(|h| h.name().eq_ignore_ascii_case(name.as_bytes()))
            .map(|h| h.value())
    }

    fn write_temp(bytes: &[u8]) -> (NamedTempFile, PathBuf, u64) {
        let mut tf = NamedTempFile::new().expect("tmp file");
        tf.write_all(bytes).expect("write tmp");
        let len = bytes.len() as u64;
        let path = tf.path().to_path_buf();
        (tf, path, len)
    }

    fn mk_ctx(
        path: PathBuf,
        total_len: u64,
        etag: Option<&str>,
        last_modified: Option<&str>,
        content_type: Option<&[u8]>,
        if_range: Option<IfRange>,
    ) -> RangeContext {
        RangeContext {
            path,
            total_len,
            etag: etag.map(|s| s.to_string()),
            last_modified_http_date: last_modified.map(|s| s.to_string()),
            content_type: content_type.map(|b| b.to_vec()),
            if_range,
        }
    }

    fn cfg_default() -> RangeConfig {
        RangeConfig::default()
    }

    #[test]
    fn parse_only_none_when_absent() {
        let req = vec![hdr(b":method", b"GET")];
        let parsed = parse_range_header_only(&req).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_only_single_and_suffix() {
        let req = vec![hdr(b"range", b"bytes=100-199,-50,300-")];
        let parsed = parse_range_header_only(&req).unwrap().unwrap();
        assert_eq!(parsed.unit, RangeUnit::Bytes);
        assert_eq!(parsed.specs.len(), 3);

        // 100-199
        assert_eq!(parsed.specs[0].start, Some(100));
        assert_eq!(parsed.specs[0].end,   Some(199));

        // -50 (suffix)
        assert_eq!(parsed.specs[1].start, None);
        assert_eq!(parsed.specs[1].end,   Some(50));

        // 300-
        assert_eq!(parsed.specs[2].start, Some(300));
        assert_eq!(parsed.specs[2].end,   None);
    }

    #[test]
    fn parse_only_unsupported_unit() {
        let req = vec![hdr(b"range", b"items=0-1")];
        let err = parse_range_header_only(&req).unwrap_err();
        matches!(err, RangeError::UnsupportedUnit);
    }

    #[test]
    fn parse_only_malformed() {
        for bad in [
            b"bytes=" as &[u8],
            b"bytes=,",
            b"bytes=100",
            b"bytes=200-100", // end < start
            b"bytes=abc-",
            b"bytes=-",
        ] {
            let req = vec![hdr(b"range", bad)];
            let err = parse_range_header_only(&req).unwrap_err();
            matches!(err, RangeError::Parse);
        }
    }

    // -------- handle_range_request: gating/none --------

    #[test]
    fn handle_no_range_returns_none() {
        let (_tf, path, len) = write_temp(b"hello world");
        let ctx = mk_ctx(path, len, None, None, None, None);
        let out = handle_range_request(&[hdr(b":method", b"GET")], &ctx, &cfg_default()).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn if_range_etag_mismatch_returns_none() {
        let (_tf, path, len) = write_temp(b"0123456789");
        let ctx = mk_ctx(
            path,
            len,
            Some("\"etag-a\""),
            None,
            None,
            Some(IfRange { etag: Some("\"etag-b\"".into()), last_modified_http_date: None }),
        );
        let req = vec![hdr(b"range", b"bytes=0-4")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap();
        assert!(out.is_none(), "mismatch must fall back to full 200 by caller");
    }

    #[test]
    fn if_range_date_mismatch_returns_none() {
        let (_tf, path, len) = write_temp(b"0123456789");
        let ctx = mk_ctx(
            path,
            len,
            None,
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
            None,
            Some(IfRange { etag: None, last_modified_http_date: Some("Thu, 22 Oct 2015 07:28:00 GMT".into()) }),
        );
        let req = vec![hdr(b"range", b"bytes=0-4")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn if_range_match_allows_range() {
        let (_tf, path, len) = write_temp(b"ABCDEFGHIJ");
        let ctx = mk_ctx(
            path.clone(),
            len,
            Some("\"same\""),
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(b"text/plain"),
            Some(IfRange { etag: Some("\"same\"".into()), last_modified_http_date: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()) }),
        );
        let req = vec![hdr(b"range", b"bytes=1-3")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();
        assert_eq!(out.status, 206);
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 1-3/10");
        assert_eq!(out.body, b"BCD");
    }

    // -------- handle_range_request: single part --------

    #[test]
    fn single_range_honors_content_type_and_content_range() {
        let (_tf, path, len) = write_temp(b"The quick brown fox");
        let ctx = mk_ctx(path, len, None, None, Some(b"text/plain"), None);
        let req = vec![hdr(b"range", b"bytes=4-8")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();

        assert_eq!(out.status, 206);
        assert_eq!(out.body, b"quick");
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 4-8/19");
        assert_eq!(get_header(&out.headers, "content-type").unwrap(), b"text/plain");
        assert_eq!(
            get_header(&out.headers, "content-length").unwrap(),
            b"5"
        );
    }

    #[test]
    fn single_open_ended_range() {
        let (_tf, path, len) = write_temp(b"0123456789");
        let ctx = mk_ctx(path, len, None, None, None, None);
        let req = vec![hdr(b"range", b"bytes=7-")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();

        assert_eq!(out.status, 206);
        assert_eq!(out.body, b"789");
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 7-9/10");
    }

    #[test]
    fn single_suffix_range() {
        let (_tf, path, len) = write_temp(b"abcdef");
        let ctx = mk_ctx(path, len, None, None, None, None);
        let req = vec![hdr(b"range", b"bytes=-3")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();

        assert_eq!(out.status, 206);
        assert_eq!(out.body, b"def");
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 3-5/6");
    }

    #[test]
    fn unsatisfiable_returns_416_and_star_len() {
        let (_tf, path, len) = write_temp(b"abc");
        let ctx = mk_ctx(path, len, None, None, None, None);
        // Start beyond EOF
        let req = vec![hdr(b"range", b"bytes=10-20")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();

        assert_eq!(out.status, 416);
        assert_eq!(out.body.len(), 0);
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes */3");
        assert_eq!(get_header(&out.headers, "content-length").unwrap(), b"0");
    }

    #[test]
    fn zero_length_representation_is_416() {
        let (_tf, path, _len) = write_temp(b"");
        // Intentionally set total_len = 0
        let ctx = mk_ctx(path, 0, None, None, None, None);
        let req = vec![hdr(b"range", b"bytes=0-0")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();
        assert_eq!(out.status, 416);
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes */0");
    }

    #[test]
    fn malformed_range_yields_parse_error() {
        let (_tf, path, len) = write_temp(b"0123");
        let ctx = mk_ctx(path, len, None, None, None, None);
        let req = vec![hdr(b"range", b"bytes=abc-")];
        let err = handle_range_request(&req, &ctx, &cfg_default()).unwrap_err();
        matches!(err, RangeError::Parse);
    }

    #[test]
    fn unsupported_unit_yields_error() {
        let (_tf, path, len) = write_temp(b"0123");
        let ctx = mk_ctx(path, len, None, None, None, None);
        let req = vec![hdr(b"range", b"items=0-1")];
        let err = handle_range_request(&req, &ctx, &cfg_default()).unwrap_err();
        matches!(err, RangeError::UnsupportedUnit);
    }

    #[test]
    fn io_error_bubbles_up() {
        // point to a non-existent file but with a plausible len so evaluation succeeds
        let bogus_path = PathBuf::from("/nonexistent/path/file.bin");
        let ctx = mk_ctx(bogus_path, 100, None, None, None, None);
        let req = vec![hdr(b"range", b"bytes=0-5")];
        let err = handle_range_request(&req, &ctx, &cfg_default()).unwrap_err();
        matches!(err, RangeError::Io(_));
    }

    // -------- handle_range_request: multipart --------

    #[test]
    fn multipart_two_ranges_with_boundary_and_parts() {
        let data = b"0123456789ABCDEFG";
        let (_tf, path, len) = write_temp(data);
        let ctx = mk_ctx(path, len, None, None, Some(b"application/octet-stream"), None);
        let req = vec![hdr(b"range", b"bytes=0-2,10-12")];

        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();
        assert_eq!(out.status, 206);

        // Ensure content-type is multipart with a boundary
        let ctype = std::str::from_utf8(get_header(&out.headers, "content-type").unwrap()).unwrap();
        assert!(ctype.starts_with("multipart/byteranges; boundary="));
        let boundary = ctype.split("boundary=").nth(1).unwrap();

        // Body should contain both opening and closing boundary lines
        let body_txt = String::from_utf8_lossy(&out.body);
        let open_marker = format!("--{}\r\n", boundary);
        let close_marker = format!("--{}--\r\n", boundary);
        assert!(body_txt.contains(&open_marker));
        assert!(body_txt.contains(&close_marker));

        // Expect two parts; count "Content-Range:" occurrences
        let cnt = body_txt.matches("Content-Range: ").count();
        assert_eq!(cnt, 2);

        // Each part must carry correct sub-headers and payloads
        assert!(body_txt.contains("Content-Type: application/octet-stream\r\n"));
        assert!(body_txt.contains("Content-Range: bytes 0-2/17\r\n\r\n012\r\n"));
        assert!(body_txt.contains("Content-Range: bytes 10-12/17\r\n\r\nABC\r\n"));

        // Content-Length header should match exact byte size of body
        let declared_len: usize = std::str::from_utf8(
            get_header(&out.headers, "content-length").unwrap()
        ).unwrap().parse().unwrap();
        assert_eq!(declared_len, out.body.len());
    }

    #[test]
    fn multipart_disabled_forces_single_first_range() {
        let data = b"Hello Rust ranges!";
        let (_tf, path, len) = write_temp(data);
        let ctx = mk_ctx(path, len, None, None, Some(b"text/plain"), None);

        // Disable multipart
        let cfg = RangeConfig { enable_multipart: false, max_parts: 16 };

        // Ask for two ranges; only first should be served as single
        let req = vec![hdr(b"range", b"bytes=0-4,6-9")];
        let out = handle_range_request(&req, &ctx, &cfg).unwrap().unwrap();

        assert_eq!(out.status, 206);
        assert_eq!(get_header(&out.headers, "content-type").unwrap(), b"text/plain");
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 0-4/18");
        assert_eq!(out.body, b"Hello");
    }

    #[test]
    fn max_parts_trimming_uses_only_first_n() {
        let data = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let (_tf, path, len) = write_temp(data);
        let ctx = mk_ctx(path, len, None, None, None, None);
        let cfg = RangeConfig { enable_multipart: true, max_parts: 1 }; // trim to 1

        let req = vec![hdr(b"range", b"bytes=0-0,2-2,4-4")];
        let out = handle_range_request(&req, &ctx, &cfg).unwrap().unwrap();

        // With multipart enabled but only 1 part after trimming, code path goes single-part
        assert_eq!(out.status, 206);
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 0-0/26");
        assert_eq!(out.body, b"A");
    }

    #[test]
    fn ranges_partially_out_of_bounds_are_clamped() {
        // Ask for 0-999 on a short file; should clamp to end
        let (_tf, path, len) = write_temp(b"short");
        let ctx = mk_ctx(path, len, None, None, None, None);
        let req = vec![hdr(b"range", b"bytes=0-999")];
        let out = handle_range_request(&req, &ctx, &cfg_default()).unwrap().unwrap();

        assert_eq!(out.status, 206);
        assert_eq!(get_header(&out.headers, "content-range").unwrap(), b"bytes 0-4/5");
        assert_eq!(out.body, b"short");
    }

    // Utility for matches! on older Rust compilers used in some CI setups.
    // (If your MSRV supports the std matches! macro, you can delete this.)
    #[allow(unused_macros)]
    macro_rules! matches {
        ($e:expr, $pat:pat $(if $guard: expr)? $(,)?) => {
            match $e { $pat $(if $guard)? => true, _ => false }
        };
    }
}
