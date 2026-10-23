//! Uploads to img.fish (https://img.fish/api): the file is POSTed as the
//! `file` field of a form to `/up`, optionally with an API key so it lands in
//! your account, and the host answers with its link and a private address
//! that deletes it again.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Deserialize;

use crate::pomf::{form, host};
use crate::upload::{Body, CANCELLED, Counting, Progress};

/// Where and how to upload.
#[derive(Debug, Clone)]
pub struct ImgFishTarget {
    /// The site, e.g. `https://img.fish`.
    pub url: String,
    /// Empty uploads anonymously.
    pub api_key: String,
}

/// The host's answer: `{"link": ..., "destroy": ...}`, or `{"error": ...}`.
#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    destroy: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl ImgFishTarget {
    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(300)))
            .build()
            .into()
    }

    /// Uploads the file as `name` (the host gives it a name of its own,
    /// keeping the extension), following along in `progress`. Returns its
    /// link and the address that deletes it.
    pub fn upload(
        &self,
        name: &str,
        body: &Body,
        content_type: &str,
        progress: &Progress,
    ) -> Result<(String, String), String> {
        let data = body.read(0, body.len()?)?;
        let boundary: String = (0..24).map(|_| fastrand::alphanumeric()).collect();
        let form = form(&boundary, "file", name, content_type, &data);
        drop(data);
        progress.total.store(form.len() as u64, Ordering::Relaxed);

        let mut counting = Counting::new(&form, progress);
        let mut request = ureq::http::Request::builder()
            .method("POST")
            .uri(format!("{}/up", self.base()))
            .header("accept", "application/json")
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .header("content-length", form.len())
            .header("user-agent", concat!("snapr/", env!("CARGO_PKG_VERSION")));
        // A pasted key often brings a stray space or newline along.
        let key = self.api_key.trim();
        if !key.is_empty() {
            request = request.header("x-api-key", key);
        }
        let request = request
            .body(ureq::SendBody::from_reader(&mut counting))
            .map_err(|e| e.to_string())?;
        let response = Self::agent().run(request);
        let sent = counting.sent;
        let result = self.read_response(response).and_then(|reply| match reply {
            Reply {
                link: Some(link),
                destroy: Some(destroy),
                ..
            } => Ok((link, destroy)),
            _ => Err("no link in the reply".into()),
        });
        if result.is_err() {
            progress.sent.fetch_sub(sent as u64, Ordering::Relaxed);
            if progress.cancelled() {
                return Err(CANCELLED.into());
            }
        }
        result
    }

    /// Deletes an upload for good, given the address [`Self::upload`]
    /// returned for it.
    pub fn destroy(&self, destroy_url: &str) -> Result<(), String> {
        let request = ureq::http::Request::builder()
            .method("DELETE")
            .uri(destroy_url.trim())
            .header("accept", "application/json")
            .header("user-agent", concat!("snapr/", env!("CARGO_PKG_VERSION")))
            .body(())
            .map_err(|e| e.to_string())?;
        match self.read_response(Self::agent().run(request))? {
            Reply { status: Some(s), .. } if s.eq_ignore_ascii_case("success") => Ok(()),
            _ => Err("the host didn't say it was deleted".into()),
        }
    }

    fn base(&self) -> &str {
        self.url.trim().trim_end_matches('/')
    }

    fn read_response(
        &self,
        response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Reply, String> {
        let mut response =
            response.map_err(|e| format!("couldn't reach {}: {e}", host(&self.url)))?;
        let status = response.status();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let reply = serde_json::from_str::<Reply>(&text);
        let why = match reply {
            Ok(Reply { error: Some(e), .. }) => e,
            Ok(reply) if status.is_success() => return Ok(reply),
            Ok(_) => "no details".into(),
            Err(_) => {
                let text = text.trim();
                if text.is_empty() {
                    "an empty reply".into()
                } else {
                    // A rate limit or proxy page; enough to recognise.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> ImgFishTarget {
        ImgFishTarget {
            url: "https://img.fish/".into(),
            api_key: String::new(),
        }
    }

    #[test]
    fn replies() {
        let reply = |status: u16, body: &str| {
            let response = ureq::http::Response::builder()
                .status(status)
                .body(ureq::Body::builder().data(body.to_string()))
                .unwrap();
            target().read_response(Ok(response))
        };
        let ok = reply(
            200,
            r#"{"link":"https://img.fish/ntj74zx8.png","destroy":"https://img.fish/destroy/abc"}"#,
        )
        .unwrap();
        assert_eq!(ok.link.as_deref(), Some("https://img.fish/ntj74zx8.png"));
        assert_eq!(ok.destroy.as_deref(), Some("https://img.fish/destroy/abc"));
        assert_eq!(
            reply(400, r#"{"error":"File type not allowed"}"#).unwrap_err(),
            "400 Bad Request: File type not allowed"
        );
        assert_eq!(
            reply(429, "Too many requests").unwrap_err(),
            "429 Too Many Requests: Too many requests"
        );
        assert_eq!(target().base(), "https://img.fish");
    }

    /// Uploads a tiny PNG to img.fish and deletes it again, with the API key
    /// in `SNAPR_IMGFISH_KEY` if it's set:
    /// `cargo test upload_to_img_fish -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn upload_to_img_fish() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let progress = Progress::default();
        let t = ImgFishTarget {
            api_key: std::env::var("SNAPR_IMGFISH_KEY").unwrap_or_default(),
            ..target()
        };
        let (link, destroy) = t
            .upload("2026-10/test.png", &Body::Bytes(png.into()), "image/png", &progress)
            .unwrap();
        println!("{link}");
        assert!(link.ends_with(".png"), "{link}");
        assert_eq!(
            progress.sent.load(Ordering::Relaxed),
            progress.total.load(Ordering::Relaxed)
        );
        t.destroy(&destroy).unwrap();
        assert!(t.destroy(&destroy).is_err());
    }
}
