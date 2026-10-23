//! Uploads to S3-compatible storage (Cloudflare R2, AWS S3, MinIO, Backblaze
//! B2, ...), with requests signed using AWS Signature Version 4.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// Bodies larger than this are sent as a multipart upload.
const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
/// Size of every part but the last: S3 needs at least 5 MiB, and R2 needs
/// all but the last part to be the same size. Bigger for huge files, which
/// would otherwise need more than `MAX_PARTS`.
const PART_SIZE: u64 = 8 * 1024 * 1024;
const MAX_PARTS: u64 = 10_000;
/// The error an upload ends with when it's cancelled.
pub const CANCELLED: &str = "cancelled";

/// What to upload: bytes in memory, or a file, read a part at a time so
/// big ones aren't loaded whole.
#[derive(Debug, Clone)]
pub enum Body {
    Bytes(Arc<Vec<u8>>),
    File(PathBuf),
}

impl Body {
    pub fn len(&self) -> Result<u64, String> {
        match self {
            Body::Bytes(b) => Ok(b.len() as u64),
            Body::File(p) => std::fs::metadata(p)
                .map(|m| m.len())
                .map_err(|e| format!("couldn't read {}: {e}", p.display())),
        }
    }

    /// `len` bytes from `start`.
    pub(crate) fn read(&self, start: u64, len: u64) -> Result<Vec<u8>, String> {
        match self {
            Body::Bytes(b) => Ok(b[start as usize..(start + len) as usize].to_vec()),
            Body::File(p) => {
                let read = || -> std::io::Result<Vec<u8>> {
                    let mut f = std::fs::File::open(p)?;
                    f.seek(SeekFrom::Start(start))?;
                    let mut buf = vec![0; len as usize];
                    f.read_exact(&mut buf)?;
                    Ok(buf)
                };
                read().map_err(|e| format!("couldn't read {}: {e}", p.display()))
            }
        }
    }
}

/// How far an upload has got; shared with whoever shows it, who can also
/// cancel it.
#[derive(Debug, Default)]
pub struct Progress {
    pub sent: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Hands a request body to ureq, counting it into `progress` as it goes.
pub(crate) struct Counting<'a> {
    data: &'a [u8],
    /// Bytes handed over so far.
    pub(crate) sent: usize,
    progress: &'a Progress,
}

impl<'a> Counting<'a> {
    pub(crate) fn new(data: &'a [u8], progress: &'a Progress) -> Self {
        Self {
            data,
            sent: 0,
            progress,
        }
    }
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.progress.cancelled() {
            return Err(std::io::Error::other(CANCELLED));
        }
        // Small steps, so the count follows the network rather than
        // jumping by whole buffers.
        let n = buf.len().min(64 * 1024).min(self.data.len() - self.sent);
        buf[..n].copy_from_slice(&self.data[self.sent..self.sent + n]);
        self.sent += n;
        self.progress.sent.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
/// Tries per part before the upload is given up.
const PART_ATTEMPTS: u32 = 3;
/// Parts uploaded at the same time, unless set otherwise.
pub const DEFAULT_CONCURRENCY: u32 = 4;
/// The most parts at once a destination can be set to; each one holds its
/// part in memory while it's sent.
pub const MAX_CONCURRENCY: u32 = 32;

/// Where an upload destination's files go, ready to send.
#[derive(Debug, Clone)]
pub enum Target {
    S3(S3Target),
    Pomf(crate::pomf::PomfTarget),
}

/// Where and how to upload.
#[derive(Debug, Clone)]
pub struct S3Target {
    /// e.g. `https://<account>.r2.cloudflarestorage.com` or `https://s3.eu-west-1.amazonaws.com`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// `endpoint/bucket/key` rather than `bucket.endpoint/key`.
    pub path_style: bool,
    /// Sign a hash of the body; otherwise it's sent as `UNSIGNED-PAYLOAD`,
    /// which skips hashing it (HTTPS already protects it in transit).
    pub sign_payload: bool,
    /// Parts of a multipart upload sent at the same time.
    pub concurrency: u32,
}

/// The parts of a URL the signer needs.
struct Url {
    scheme: String,
    /// Host, with the port if it isn't the scheme's default.
    host: String,
    /// Already-encoded path, starting with `/`.
    path: String,
}

impl S3Target {
    fn url(&self, key: &str) -> Result<Url, String> {
        let (scheme, rest) = self
            .endpoint
            .trim()
            .trim_end_matches('/')
            .split_once("://")
            .ok_or_else(|| format!("endpoint {:?} should start with https://", self.endpoint))?;
        let (host, base_path) = match rest.split_once('/') {
            Some((h, p)) => (h, format!("/{}", p.trim_matches('/'))),
            None => (rest, String::new()),
        };
        if host.is_empty() {
            return Err(format!("endpoint {:?} has no host", self.endpoint));
        }
        let key = encode_path(key);
        let bucket = encode_segment(&self.bucket);
        Ok(if self.path_style {
            Url {
                scheme: scheme.into(),
                host: host.into(),
                path: format!("{base_path}/{bucket}/{key}"),
            }
        } else {
            Url {
                scheme: scheme.into(),
                host: format!("{}.{host}", self.bucket),
                path: format!("{base_path}/{key}"),
            }
        })
    }

    /// The object's URL on the storage endpoint itself.
    pub fn object_url(&self, key: &str) -> Result<String, String> {
        let url = self.url(key)?;
        Ok(format!("{}://{}{}", url.scheme, url.host, url.path))
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into()
    }

    /// Uploads an object, as a multipart upload if it's large, following
    /// along in `progress`.
    pub fn put_object(
        &self,
        key: &str,
        body: &Body,
        content_type: &str,
        progress: &Progress,
    ) -> Result<(), String> {
        let len = body.len()?;
        progress.total.store(len, Ordering::Relaxed);
        let result = if len > MULTIPART_THRESHOLD {
            let part_size = PART_SIZE.max(len.div_ceil(MAX_PARTS).next_multiple_of(1024 * 1024));
            self.put_multipart(key, body, len, content_type, part_size, progress)
        } else {
            let data = body.read(0, len)?;
            self.send("PUT", key, &[], &data, Some(content_type), Some(progress))
                .map(drop)
        };
        match result {
            Err(_) if progress.cancelled() => Err(CANCELLED.into()),
            r => r,
        }
    }

    pub fn delete_object(&self, key: &str) -> Result<(), String> {
        self.send("DELETE", key, &[], &[], None, None).map(drop)
    }

    /// Uploads `body` (`len` bytes) in `part_size` pieces, several at a
    /// time. If it fails, the upload is aborted so its parts don't linger
    /// (and get billed) in the bucket.
    fn put_multipart(
        &self,
        key: &str,
        body: &Body,
        len: u64,
        content_type: &str,
        part_size: u64,
        progress: &Progress,
    ) -> Result<(), String> {
        let created =
            self.send("POST", key, &[("uploads", "")], &[], Some(content_type), None)?;
        let upload_id = xml_tag(&created.body, "UploadId")
            .ok_or("starting multipart upload: no UploadId in the response")?;
        let result = (|| {
            let parts: Vec<(u64, u64)> = (0..len.div_ceil(part_size))
                .map(|i| {
                    let start = i * part_size;
                    (start, part_size.min(len - start))
                })
                .collect();
            let etags = self.upload_parts(key, &upload_id, body, &parts, progress)?;
            let mut parts = String::new();
            for (i, etag) in etags.iter().enumerate() {
                let _ = write!(
                    parts,
                    "<Part><PartNumber>{}</PartNumber><ETag>{etag}</ETag></Part>",
                    i + 1
                );
            }
            let xml = format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>");
            let done = self.send(
                "POST",
                key,
                &[("uploadId", &upload_id)],
                xml.as_bytes(),
                Some("application/xml"),
                None,
            )?;
            // A completion can fail after the 200 status has been sent.
            if done.body.contains("<Error>") {
                return Err(s3_error(&done.body));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.send("DELETE", key, &[("uploadId", &upload_id)], &[], None, None);
        }
        result
    }

    /// Uploads the parts (start, length), up to `concurrency` at a
    /// time, and returns their ETags in order. Stops handing out parts after
    /// the first failure or a cancel.
    fn upload_parts(
        &self,
        key: &str,
        upload_id: &str,
        body: &Body,
        parts: &[(u64, u64)],
        progress: &Progress,
    ) -> Result<Vec<String>, String> {
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let etags: Vec<Mutex<Option<String>>> = parts.iter().map(|_| Mutex::new(None)).collect();
        let error = Mutex::new(None);
        thread::scope(|scope| {
            let workers = self.concurrency.clamp(1, MAX_CONCURRENCY) as usize;
            for _ in 0..workers.min(parts.len()) {
                scope.spawn(|| {
                    while !failed.load(Ordering::Relaxed) && !progress.cancelled() {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(start, len)) = parts.get(i) else { break };
                        let part = body
                            .read(start, len)
                            .and_then(|data| self.upload_part(key, upload_id, i + 1, &data, progress));
                        match part {
                            Ok(etag) => *etags[i].lock().unwrap() = Some(etag),
                            Err(e) => {
                                failed.store(true, Ordering::Relaxed);
                                error.lock().unwrap().get_or_insert(e);
                            }
                        }
                    }
                });
            }
        });
        if let Some(e) = error.into_inner().unwrap() {
            return Err(e);
        }
        if progress.cancelled() {
            return Err(CANCELLED.into());
        }
        Ok(etags
            .into_iter()
            .map(|e| e.into_inner().unwrap().expect("every part uploaded"))
            .collect())
    }

    /// Uploads one part, retrying a few times; returns its ETag.
    fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        n: usize,
        chunk: &[u8],
        progress: &Progress,
    ) -> Result<String, String> {
        let n = n.to_string();
        let query = [("partNumber", n.as_str()), ("uploadId", upload_id)];
        let mut attempt = 1;
        loop {
            match self.send("PUT", key, &query, chunk, None, Some(progress)) {
                Ok(r) => return r.etag.ok_or_else(|| format!("part {n}: no ETag")),
                Err(_) if progress.cancelled() => return Err(CANCELLED.into()),
                Err(_) if attempt < PART_ATTEMPTS => {
                    thread::sleep(Duration::from_secs(attempt as u64));
                    attempt += 1;
                }
                Err(e) => return Err(format!("part {n}: {e}")),
            }
        }
    }

    /// Sends one signed request. With `progress`, the body is counted into
    /// it as it's sent (and taken back out if the request fails).
    fn send(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, &str)],
        body: &[u8],
        content_type: Option<&str>,
        progress: Option<&Progress>,
    ) -> Result<Response, String> {
        let url = self.url(key)?;
        let query = canonical_query(query);
        let payload_hash = if self.sign_payload {
            hex(&Sha256::digest(body))
        } else {
            "UNSIGNED-PAYLOAD".into()
        };
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let mut headers = vec![
            ("host".to_string(), url.host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ];
        if let Some(ct) = content_type {
            headers.push(("content-type".to_string(), ct.to_string()));
        }
        headers.sort();
        let auth = authorization(&Request {
            method,
            path: &url.path,
            query: &query,
            headers: &headers,
            payload_hash: &payload_hash,
            time: now,
            region: &self.region,
            access_key_id: &self.access_key_id,
            secret_access_key: &self.secret_access_key,
        });

        let mut full_url = format!("{}://{}{}", url.scheme, url.host, url.path);
        if !query.is_empty() {
            full_url = format!("{full_url}?{query}");
        }
        let agent = Self::agent();
        let mut request = ureq::http::Request::builder().method(method).uri(&full_url);
        for (name, value) in headers.iter().filter(|(n, _)| n != "host") {
            request = request.header(name, value);
        }
        let request = request.header("authorization", auth);
        let mut counting = progress.map(|progress| Counting {
            data: body,
            sent: 0,
            progress,
        });
        let response = match (method, &mut counting) {
            ("DELETE", _) => agent.run(request.body(()).map_err(|e| e.to_string())?),
            // A set length, as S3 doesn't take chunked uploads.
            (_, Some(c)) => agent.run(
                request
                    .header("content-length", body.len())
                    .body(ureq::SendBody::from_reader(c))
                    .map_err(|e| e.to_string())?,
            ),
            (_, None) => agent.run(request.body(body).map_err(|e| e.to_string())?),
        };
        let result = Self::read_response(response, &url.host);
        if result.is_err()
            && let Some(c) = &counting
        {
            c.progress.sent.fetch_sub(c.sent as u64, Ordering::Relaxed);
        }
        result
    }

    fn read_response(
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
        host: &str,
    ) -> Result<Response, String> {
        let mut response = response.map_err(|e| format!("couldn't reach {host}: {e}"))?;
        let status = response.status();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = response.body_mut().read_to_string().unwrap_or_default();
        if status.is_success() {
            return Ok(Response { etag, body: text });
        }
        Err(format!(
            "{} {}: {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            s3_error(&text)
        ))
    }
}

/// What the request methods need from a successful response.
struct Response {
    etag: Option<String>,
    body: String,
}

/// Sorted, encoded `name=value` pairs, for both the URL and the signature.
fn canonical_query(pairs: &[(&str, &str)]) -> String {
    let mut pairs: Vec<_> = pairs
        .iter()
        .map(|(n, v)| (encode_segment(n), encode_segment(v)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(n, v)| format!("{n}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The text inside the first `<name>` element.
fn xml_tag(body: &str, name: &str) -> Option<String> {
    let start = body.find(&format!("<{name}>"))? + name.len() + 2;
    let end = body[start..].find(&format!("</{name}>"))? + start;
    Some(body[start..end].to_string())
}

/// Pulls `Code: Message` out of an S3 XML error body.
fn s3_error(body: &str) -> String {
    match (xml_tag(body, "Code"), xml_tag(body, "Message")) {
        (Some(code), Some(msg)) => format!("{code}: {msg}"),
        (Some(code), None) => code,
        _ if body.trim().is_empty() => "no details".into(),
        _ => body.chars().take(200).collect(),
    }
}

struct Request<'a> {
    method: &'a str,
    /// Encoded path.
    path: &'a str,
    /// Canonical query string (already sorted and encoded).
    query: &'a str,
    /// Lower-case names, sorted, values trimmed.
    headers: &'a [(String, String)],
    payload_hash: &'a str,
    time: DateTime<Utc>,
    region: &'a str,
    access_key_id: &'a str,
    secret_access_key: &'a str,
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The SigV4 `Authorization` header value for an S3 request.
fn authorization(r: &Request) -> String {
    let date = r.time.format("%Y%m%d").to_string();
    let amz_date = r.time.format("%Y%m%dT%H%M%SZ").to_string();
    let canonical_headers: String = r
        .headers
        .iter()
        .map(|(n, v)| format!("{n}:{}\n", v.trim()))
        .collect();
    let signed_headers = r
        .headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        r.method, r.path, r.query, r.payload_hash
    );
    let scope = format!("{date}/{}/s3/aws4_request", r.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac(format!("AWS4{}", r.secret_access_key).as_bytes(), &date);
    let k_region = hmac(&k_date, r.region);
    let k_service = hmac(&k_region, "s3");
    let k_signing = hmac(&k_service, "aws4_request");
    let signature = hex(&hmac(&k_signing, &string_to_sign));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        r.access_key_id
    )
}

/// URI-encodes one path segment the way S3 expects (RFC 3986 unreserved
/// characters are kept, everything else is `%XX`).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Encodes an object key, keeping `/` between segments.
pub fn encode_path(key: &str) -> String {
    key.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const AK: &str = "AKIAIOSFODNN7EXAMPLE";
    const SK: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut h: Vec<_> = pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        h.sort();
        h
    }

    /// "Example: GET Object" from the AWS SigV4 for S3 documentation.
    #[test]
    fn matches_aws_get_object_example() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let auth = authorization(&Request {
            method: "GET",
            path: "/test.txt",
            query: "",
            headers: &headers(&[
                ("host", "examplebucket.s3.amazonaws.com"),
                ("range", "bytes=0-9"),
                ("x-amz-content-sha256", empty),
                ("x-amz-date", "20130524T000000Z"),
            ]),
            payload_hash: empty,
            time: Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
            region: "us-east-1",
            access_key_id: AK,
            secret_access_key: SK,
        });
        assert!(
            auth.ends_with(
                "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            ),
            "{auth}"
        );
        assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }

    /// "Example: PUT Object" from the same documentation, including a key
    /// that needs encoding.
    #[test]
    fn matches_aws_put_object_example() {
        let body_hash = hex(&Sha256::digest(b"Welcome to Amazon S3."));
        assert_eq!(
            body_hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let auth = authorization(&Request {
            method: "PUT",
            path: &format!("/{}", encode_path("test$file.text")),
            query: "",
            headers: &headers(&[
                ("date", "Fri, 24 May 2013 00:00:00 GMT"),
                ("host", "examplebucket.s3.amazonaws.com"),
                ("x-amz-content-sha256", &body_hash),
                ("x-amz-date", "20130524T000000Z"),
                ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ]),
            payload_hash: &body_hash,
            time: Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
            region: "us-east-1",
            access_key_id: AK,
            secret_access_key: SK,
        });
        assert!(
            auth.ends_with(
                "Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
            ),
            "{auth}"
        );
    }

    /// "Example: GET Bucket (List Objects)", which signs a query string.
    #[test]
    fn matches_aws_list_objects_example() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let query = canonical_query(&[("prefix", "J"), ("max-keys", "2")]);
        assert_eq!(query, "max-keys=2&prefix=J");
        let auth = authorization(&Request {
            method: "GET",
            path: "/",
            query: &query,
            headers: &headers(&[
                ("host", "examplebucket.s3.amazonaws.com"),
                ("x-amz-content-sha256", empty),
                ("x-amz-date", "20130524T000000Z"),
            ]),
            payload_hash: empty,
            time: Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
            region: "us-east-1",
            access_key_id: AK,
            secret_access_key: SK,
        });
        assert!(
            auth.ends_with(
                "Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
            ),
            "{auth}"
        );
    }

    #[test]
    fn encodes_multipart_queries() {
        assert_eq!(canonical_query(&[("uploads", "")]), "uploads=");
        assert_eq!(
            canonical_query(&[("uploadId", "a/b+c=="), ("partNumber", "2")]),
            "partNumber=2&uploadId=a%2Fb%2Bc%3D%3D"
        );
    }

    #[test]
    fn builds_path_and_virtual_host_urls() {
        let mut t = S3Target {
            endpoint: "https://acct.r2.cloudflarestorage.com/".into(),
            region: "auto".into(),
            bucket: "shots".into(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            path_style: true,
            sign_payload: true,
            concurrency: DEFAULT_CONCURRENCY,
        };
        assert_eq!(
            t.object_url("2026-10/a b.png").unwrap(),
            "https://acct.r2.cloudflarestorage.com/shots/2026-10/a%20b.png"
        );
        t.path_style = false;
        assert_eq!(
            t.object_url("x.png").unwrap(),
            "https://shots.acct.r2.cloudflarestorage.com/x.png"
        );
        t.endpoint = "http://localhost:9000".into();
        t.path_style = true;
        assert_eq!(
            t.object_url("x.png").unwrap(),
            "http://localhost:9000/shots/x.png"
        );
        t.endpoint = "acct.r2.cloudflarestorage.com".into();
        assert!(t.object_url("x.png").is_err());
    }

    #[test]
    fn reads_s3_error_bodies() {
        let body = "<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
        assert_eq!(s3_error(body), "AccessDenied: Access Denied");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Uploads to a real bucket (in one go, then in parts) and deletes the
    /// objects again. Set
    /// SNAPR_TEST_S3_ENDPOINT, _REGION, _BUCKET, _ACCESS_KEY_ID and
    /// _SECRET_ACCESS_KEY (and _PATH_STYLE=1 for R2/MinIO, _SIGN_PAYLOAD=1 to hash the body), then run
    /// `cargo test live_bucket -- --ignored`.
    #[test]
    #[ignore]
    fn live_bucket_round_trip() {
        let var = |n: &str| {
            std::env::var(format!("SNAPR_TEST_S3_{n}"))
                .unwrap_or_else(|_| panic!("set SNAPR_TEST_S3_{n}"))
        };
        let target = S3Target {
            endpoint: var("ENDPOINT"),
            region: std::env::var("SNAPR_TEST_S3_REGION").unwrap_or_else(|_| "auto".into()),
            bucket: var("BUCKET"),
            access_key_id: var("ACCESS_KEY_ID"),
            secret_access_key: var("SECRET_ACCESS_KEY"),
            path_style: std::env::var("SNAPR_TEST_S3_PATH_STYLE").is_ok_and(|v| v == "1"),
            sign_payload: std::env::var("SNAPR_TEST_S3_SIGN_PAYLOAD").is_ok_and(|v| v == "1"),
            concurrency: DEFAULT_CONCURRENCY,
        };
        let key = format!("snapr-live-test/{} file.txt", fastrand::u32(..));
        let body = Body::Bytes(Arc::new(b"hello from snapr".to_vec()));
        target
            .put_object(&key, &body, "text/plain", &Progress::default())
            .unwrap();
        target.delete_object(&key).unwrap();

        // Three parts at S3's 5 MiB minimum, the last one shorter.
        let len = 11 * 1024 * 1024;
        let body = Body::Bytes(Arc::new((0..len).map(|i| (i % 251) as u8).collect()));
        let key = format!("snapr-live-test/{} multipart.bin", fastrand::u32(..));
        let progress = Progress::default();
        target
            .put_multipart(
                &key,
                &body,
                len as u64,
                "application/octet-stream",
                5 * 1024 * 1024,
                &progress,
            )
            .unwrap();
        assert_eq!(progress.sent.load(Ordering::Relaxed), len as u64);
        target.delete_object(&key).unwrap();
    }
}

/// Uploads against a fake S3 on localhost.
#[cfg(test)]
mod fake {
    use super::*;

    /// Enough of S3 for uploads: multipart create/part/complete/abort and
    /// plain PUTs. Records each request's method, query and body length,
    /// and fails any body that isn't sent with a Content-Length.
    /// Each request: method, query, body length.
    type Log = Arc<Mutex<Vec<(String, String, usize)>>>;

    fn fake_s3() -> (S3Target, Log) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let requests = log.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { break };
                let requests = requests.clone();
                thread::spawn(move || {
                    let mut out = conn.try_clone().unwrap();
                    let mut r = BufReader::new(conn);
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut words = line.split_whitespace();
                        let method = words.next().unwrap_or("").to_string();
                        let target = words.next().unwrap_or("").to_string();
                        let query = target.split_once('?').map_or("", |(_, q)| q).to_string();
                        let (mut len, mut chunked) = (0usize, false);
                        loop {
                            let mut h = String::new();
                            r.read_line(&mut h).unwrap();
                            let h = h.trim_end().to_ascii_lowercase();
                            if h.is_empty() {
                                break;
                            }
                            if let Some(v) = h.strip_prefix("content-length:") {
                                len = v.trim().parse().unwrap();
                            }
                            chunked |= h.starts_with("transfer-encoding:");
                        }
                        let mut body = vec![0; len];
                        r.read_exact(&mut body).unwrap();
                        requests.lock().unwrap().push((method.clone(), query.clone(), len));
                        let (status, extra, text) = if chunked {
                            ("411 Length Required", String::new(), String::new())
                        } else if method == "POST" && query == "uploads=" {
                            let xml = "<InitiateMultipartUploadResult><UploadId>up1</UploadId></InitiateMultipartUploadResult>";
                            ("200 OK", String::new(), xml.to_string())
                        } else if method == "PUT" {
                            ("200 OK", format!("ETag: \"e{len}\"\r\n"), String::new())
                        } else if method == "DELETE" {
                            ("204 No Content", String::new(), String::new())
                        } else {
                            ("200 OK", String::new(), "<CompleteMultipartUploadResult/>".into())
                        };
                        let _ = write!(
                            out,
                            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\n\r\n{text}",
                            text.len()
                        );
                    }
                });
            }
        });
        let target = S3Target {
            endpoint: format!("http://127.0.0.1:{port}"),
            region: "auto".into(),
            bucket: "b".into(),
            access_key_id: "test".into(),
            secret_access_key: "test".into(),
            path_style: true,
            sign_payload: false,
            concurrency: DEFAULT_CONCURRENCY,
        };
        (target, log)
    }

    #[test]
    fn streams_a_big_file_in_parts_with_progress() {
        let (target, log) = fake_s3();
        let path = std::env::temp_dir().join(format!("snapr-upload-{}.bin", fastrand::u32(..)));
        let len = 3 * PART_SIZE + 12345;
        std::fs::write(&path, vec![7u8; len as usize]).unwrap();
        let progress = Progress::default();
        let result = target.put_object("big.bin", &Body::File(path.clone()), "x/y", &progress);
        std::fs::remove_file(&path).unwrap();
        result.unwrap();
        assert_eq!(progress.total.load(Ordering::Relaxed), len);
        assert_eq!(progress.sent.load(Ordering::Relaxed), len);
        let log = log.lock().unwrap();
        let parts: Vec<usize> = log
            .iter()
            .filter(|(m, q, _)| m == "PUT" && q.contains("partNumber"))
            .map(|r| r.2)
            .collect();
        assert_eq!(parts.len(), 4, "{log:?}");
        assert_eq!(parts.iter().sum::<usize>() as u64, len);
        assert!(log.iter().any(|(m, q, _)| m == "POST" && q.starts_with("uploadId")));
    }

    #[test]
    fn small_upload_is_counted() {
        let (target, _) = fake_s3();
        let progress = Progress::default();
        let body = Body::Bytes(Arc::new(vec![1; 300_000]));
        target.put_object("small.png", &body, "image/png", &progress).unwrap();
        assert_eq!(progress.sent.load(Ordering::Relaxed), 300_000);
    }

    #[test]
    fn cancelling_aborts_the_multipart_upload() {
        let (target, log) = fake_s3();
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        let body = Body::Bytes(Arc::new(vec![0; (MULTIPART_THRESHOLD + 1) as usize]));
        let err = target.put_object("big.bin", &body, "x/y", &progress).unwrap_err();
        assert_eq!(err, CANCELLED);
        let log = log.lock().unwrap();
        assert!(!log.iter().any(|(_, q, _)| q.contains("partNumber")), "{log:?}");
        assert!(log.iter().any(|(m, q, _)| m == "DELETE" && q.starts_with("uploadId")));
    }

    #[test]
    fn huge_files_get_bigger_parts() {
        let len: u64 = 200 * 1024 * 1024 * 1024; // 200 GB
        let part = PART_SIZE.max(len.div_ceil(MAX_PARTS).next_multiple_of(1024 * 1024));
        assert!(len.div_ceil(part) <= MAX_PARTS);
        assert_eq!(PART_SIZE.max(6_000_000_000u64.div_ceil(MAX_PARTS)), PART_SIZE);
    }
}
