//! Uploads to Pomf-compatible file hosts (pomf.lain.la, uguu.se, qu.ax, ...):
//! the file is POSTed as the `files[]` field of a form, and the host answers
//! with JSON saying where it went. Pomf has no way to delete an upload.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Deserialize;

use crate::upload::{Body, CANCELLED, Counting, Progress};

/// Where and how to upload.
#[derive(Debug, Clone)]
pub struct PomfTarget {
    /// The upload address, e.g. `https://pomf.lain.la/upload.php`.
    pub url: String,
    /// Base for links in place of the one the host gives, e.g. a domain of
    /// your own; empty keeps the host's.
    pub public_url: String,
}

/// The host's answer: `{"success": true, "files": [{"url": ...}]}`, or
/// `{"success": false, "errorcode": 400, "description": ...}`.
#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    files: Vec<File>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct File {
    url: String,
}

impl PomfTarget {
    /// Uploads the file as `name` (the host usually renames it, keeping the
    /// extension), following along in `progress`. Returns its link.
    pub fn upload(
        &self,
        name: &str,
        body: &Body,
        content_type: &str,
        progress: &Progress,
    ) -> Result<String, String> {
        let data = body.read(0, body.len()?)?;
        let boundary: String = (0..24).map(|_| fastrand::alphanumeric()).collect();
        let form = form(&boundary, name, content_type, &data);
        drop(data);
        progress.total.store(form.len() as u64, Ordering::Relaxed);

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            // Some hosts take a while to answer after a big file.
            .timeout_recv_response(Some(Duration::from_secs(300)))
            .build()
            .into();
        let mut counting = Counting::new(&form, progress);
        let request = ureq::http::Request::builder()
            .method("POST")
            .uri(self.url.trim())
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .header("content-length", form.len())
            .header("user-agent", concat!("snapr/", env!("CARGO_PKG_VERSION")))
            .body(ureq::SendBody::from_reader(&mut counting))
            .map_err(|e| e.to_string())?;
        let response = agent.run(request);
        let sent = counting.sent;
        let result = self.read_response(response);
        if result.is_err() {
            progress.sent.fetch_sub(sent as u64, Ordering::Relaxed);
            if progress.cancelled() {
                return Err(CANCELLED.into());
            }
        }
        result
    }

    fn read_response(
        &self,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<String, String> {
        let mut response =
            response.map_err(|e| format!("couldn't reach {}: {e}", host(&self.url)))?;
        let status = response.status();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let reply = serde_json::from_str::<Reply>(&text);
        if let Ok(Reply { success: true, files, .. }) = &reply
            && let Some(file) = files.first()
        {
            return Ok(self.link(&file.url));
        }
        let why = match reply {
            Ok(Reply { description: Some(d), .. }) => d,
            Ok(_) => "no file in the reply".into(),
            Err(_) => {
                let text = text.trim();
                if text.is_empty() {
                    "an empty reply".into()
                } else {
                    // An HTML error page or the like; enough to recognise.
                    text.chars().take(200).collect()
                }
            }
        };
        Err(if status.is_success() {
            why
        } else {
            format!(
                "{} {}: {why}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            )
        })
    }

    /// The link for the `url` the host gave: as it is, under the public URL
    /// if there is one, or under the host's address if it's relative.
    fn link(&self, url: &str) -> String {
        let public = self.public_url.trim().trim_end_matches('/');
        if !public.is_empty() {
            let name = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
            return format!("{public}/{name}");
        }
        if url.starts_with("https://") || url.starts_with("http://") {
            return url.to_string();
        }
        let upload = self.url.trim();
        let origin = match upload.split_once("://") {
            Some((scheme, rest)) => format!("{scheme}://{}", rest.split('/').next().unwrap_or("")),
            None => upload.to_string(),
        };
        format!("{origin}/{}", url.trim_start_matches('/'))
    }
}

/// A `multipart/form-data` body with the file as its `files[]` field.
fn form(boundary: &str, name: &str, content_type: &str, data: &[u8]) -> Vec<u8> {
    // Only the last part of a key like `2026-10/abc.png`; no quotes or line
    // breaks, which would end the header.
    let name: String = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\\'))
        .collect();
    let head = format!(
        "--{boundary}\r\n\
         Content-Disposition: form-data; name=\"files[]\"; filename=\"{name}\"\r\n\
         Content-Type: {content_type}\r\n\r\n"
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    let mut form = Vec::with_capacity(head.len() + data.len() + tail.len());
    form.extend_from_slice(head.as_bytes());
    form.extend_from_slice(data);
    form.extend_from_slice(tail.as_bytes());
    form
}

/// The host of an address, for messages.
fn host(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(public_url: &str) -> PomfTarget {
        PomfTarget {
            url: "https://pomf.example/upload.php".into(),
            public_url: public_url.into(),
        }
    }

    #[test]
    fn links() {
        let t = target("");
        assert_eq!(t.link("https://a.pomf.example/abc.png"), "https://a.pomf.example/abc.png");
        assert_eq!(t.link("abc.png"), "https://pomf.example/abc.png");
        assert_eq!(t.link("/f/abc.png"), "https://pomf.example/f/abc.png");
        let t = target("https://i.example.com/");
        assert_eq!(t.link("https://a.pomf.example/abc.png"), "https://i.example.com/abc.png");
        assert_eq!(t.link("abc.png"), "https://i.example.com/abc.png");
    }

    #[test]
    fn form_names_the_file() {
        let form = form("XYZ", "2026-10/a\"b.png", "image/png", b"PNG");
        let text = String::from_utf8(form).unwrap();
        assert_eq!(
            text,
            "--XYZ\r\nContent-Disposition: form-data; name=\"files[]\"; filename=\"ab.png\"\r\n\
             Content-Type: image/png\r\n\r\nPNG\r\n--XYZ--\r\n"
        );
    }

    /// Uploads a tiny PNG to the Pomf host at `SNAPR_POMF_URL`:
    /// `SNAPR_POMF_URL=https://... cargo test upload_to_pomf -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn upload_to_pomf() {
        let url = std::env::var("SNAPR_POMF_URL").expect("set SNAPR_POMF_URL");
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let progress = Progress::default();
        let t = PomfTarget {
            url,
            public_url: String::new(),
        };
        let link = t
            .upload("2026-10/test.png", &Body::Bytes(png.into()), "image/png", &progress)
            .unwrap();
        println!("{link}");
        assert_eq!(
            progress.sent.load(Ordering::Relaxed),
            progress.total.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn replies() {
        let ok = |status: u16, body: &str| {
            let response = ureq::http::Response::builder()
                .status(status)
                .body(ureq::Body::builder().data(body.to_string()))
                .unwrap();
            target("").read_response(Ok(response))
        };
        assert_eq!(
            ok(
                200,
                r#"{"success":true,"files":[{"hash":"x","name":"a.png","url":"https:\/\/a.pomf.example\/q.png","size":3}]}"#
            ),
            Ok("https://a.pomf.example/q.png".into())
        );
        assert_eq!(
            ok(400, r#"{"success":false,"errorcode":400,"description":"No input file(s)"}"#),
            Err("400 Bad Request: No input file(s)".into())
        );
        assert_eq!(ok(200, r#"{"success":true,"files":[]}"#), Err("no file in the reply".into()));
        assert_eq!(
            ok(413, "<html>Too big</html>"),
            Err("413 Payload Too Large: <html>Too big</html>".into())
        );
    }
}
