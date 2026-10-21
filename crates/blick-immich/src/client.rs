use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use reqwest::multipart::{Form, Part};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use sha1::{Digest, Sha1};
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

use crate::error::{Error, Result};
use crate::models::*;
use crate::time::rfc3339;

/// Mehr gleichzeitige Thumbnail-Abrufe bringen nichts: Immich rechnet
/// Previews are sometimes generated on demand, and the server shouldn't fall over.
const THUMB_PARALLEL: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbSize {
    /// about 250 px, for the grid
    Thumbnail,
    /// about 1440 px, for the single view
    Preview,
    /// Full resolution as a web format. Immich 3.x redirects JPEG, PNG and the like
    /// to the original; HEIC and RAW only work if the server generates fullsize
    /// images, otherwise you get the preview again.
    Fullsize,
}

impl ThumbSize {
    fn as_str(self) -> &'static str {
        match self {
            ThumbSize::Thumbnail => "thumbnail",
            ThumbSize::Preview => "preview",
            ThumbSize::Fullsize => "fullsize",
        }
    }
}

/// Cheap to clone; all clones share the connection pool and the thumbnail limit.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    key: String,
    thumbs: Arc<Semaphore>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// `server` may be given with or without `/api`.
    pub fn new(server: &str, api_key: &str) -> Result<Self> {
        let server = server.trim().trim_end_matches('/');
        if !(server.starts_with("http://") || server.starts_with("https://")) {
            return Err(Error::BadUrl(server.to_owned()));
        }
        // reqwest ships no crypto provider; ring builds in the Flatpak SDK without
        // special requests (aws-lc doesn't). Installing it twice is harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let base = if server.ends_with("/api") {
            server.to_owned()
        } else {
            format!("{server}/api")
        };
        let http = reqwest::Client::builder()
            .user_agent(concat!("Blick/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            http,
            base,
            key: api_key.trim().to_owned(),
            thumbs: Arc::new(Semaphore::new(THUMB_PARALLEL)),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Address for playing a video: Immich's browser version,
    /// with range requests. The player sends the key as `x-api-key`.
    pub fn playback_url(&self, asset_id: &str) -> String {
        format!("{}/assets/{asset_id}/video/playback", self.base)
    }

    pub fn api_key(&self) -> &str {
        &self.key
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(self.url(path)).header("x-api-key", &self.key)
    }

    async fn check(resp: Response) -> Result<Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status == StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }
        let text = resp.text().await.unwrap_or_default();
        // Immich reports errors as {"message": "...", "statusCode": ...}
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("message")
                    .map(|m| m.to_string().trim_matches('"').to_owned())
            })
            .unwrap_or(text);
        Err(Error::Api {
            status: status.as_u16(),
            message,
        })
    }

    async fn json<T: DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T> {
        let bytes = Self::check(req.send().await?).await?.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Reachability, without logging in.
    pub async fn ping(&self) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct Pong {
            res: String,
        }
        let pong: Pong = Self::json(self.http.get(self.url("/server/ping"))).await?;
        if pong.res == "pong" {
            Ok(())
        } else {
            Err(Error::Api {
                status: 200,
                message: format!("ping: {}", pong.res),
            })
        }
    }

    /// Checks the key and says who it belongs to.
    pub async fn me(&self) -> Result<User> {
        Self::json(self.get("/users/me")).await
    }

    pub async fn albums(&self) -> Result<Vec<Album>> {
        let mut albums: Vec<Album> = Self::json(self.get("/albums")).await?;
        albums.sort_by_key(|a| a.album_name.to_lowercase());
        Ok(albums)
    }

    /// Months of an album (or the whole library), newest first.
    pub async fn time_buckets(&self, album_id: Option<&str>) -> Result<Vec<TimeBucket>> {
        let mut q = vec![("order", "desc")];
        if let Some(id) = album_id {
            q.push(("albumId", id));
        }
        Self::json(self.get("/timeline/buckets").query(&q)).await
    }

    pub async fn bucket_assets(
        &self,
        album_id: Option<&str>,
        bucket: &str,
    ) -> Result<Vec<RemoteAsset>> {
        let mut q = vec![("timeBucket", bucket), ("order", "desc")];
        if let Some(id) = album_id {
            q.push(("albumId", id));
        }
        let cols: BucketColumns = Self::json(self.get("/timeline/bucket").query(&q)).await?;
        Ok(cols.into_assets())
    }

    pub async fn thumbnail(&self, asset_id: &str, size: ThumbSize) -> Result<Bytes> {
        let _permit = self
            .thumbs
            .acquire()
            .await
            .expect("semaphore is never closed");
        let req = self
            .get(&format!("/assets/{asset_id}/thumbnail"))
            .query(&[("size", size.as_str())]);
        Ok(Self::check(req.send().await?).await?.bytes().await?)
    }

    /// Uploads a file. The file is streamed, not read into memory whole.
    /// If the server already knows the checksum, `UploadStatus::Duplicate` comes back.
    pub async fn upload(&self, path: &Path) -> Result<UploadResponse> {
        let io = |e| Error::Io {
            path: path.to_owned(),
            source: e,
        };
        let meta = tokio::fs::metadata(path).await.map_err(io)?;
        let checksum = sha1_hex(path).await?;
        let modified = meta.modified().unwrap_or(SystemTime::now());
        let created = meta.created().unwrap_or(modified);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mime = mime_guess::from_path(path).first_or_octet_stream();

        let file = tokio::fs::File::open(path).await.map_err(io)?;
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
        let part = Part::stream_with_length(body, meta.len())
            .file_name(name.clone())
            .mime_str(mime.as_ref())?;
        let form = Form::new()
            .text("fileCreatedAt", rfc3339(created))
            .text("fileModifiedAt", rfc3339(modified))
            .text("filename", name)
            .part("assetData", part);

        let req = self
            .http
            .post(self.url("/assets"))
            .header("x-api-key", &self.key)
            .header("x-immich-checksum", checksum)
            .multipart(form);
        Self::json(req).await
    }

    /// Name of the file as it was uploaded.
    pub async fn original_file_name(&self, asset_id: &str) -> Result<String> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Asset {
            original_file_name: String,
        }
        let a: Asset = Self::json(self.get(&format!("/assets/{asset_id}"))).await?;
        Ok(a.original_file_name)
    }

    /// Streams the original to `dst` (through a temporary file next to it).
    pub async fn download_original(&self, asset_id: &str, dst: &Path) -> Result<u64> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let io = |e| Error::Io {
            path: dst.to_owned(),
            source: e,
        };
        let resp = Self::check(
            self.get(&format!("/assets/{asset_id}/original"))
                .send()
                .await?,
        )
        .await?;
        let tmp = dst.with_extension("blick-download");
        let mut file = tokio::fs::File::create(&tmp).await.map_err(io)?;
        let mut stream = resp.bytes_stream();
        let mut n = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await.map_err(io)?;
            n += chunk.len() as u64;
        }
        file.flush().await.map_err(io)?;
        drop(file);
        tokio::fs::rename(&tmp, dst).await.map_err(io)?;
        Ok(n)
    }

    /// One page of the metadata search (1000 hits), with checksum and EXIF.
    /// `next` is the page number from the previous response, `None` for the first.
    /// Returns the hits and the next page, if there is one.
    pub async fn search_page(
        &self,
        album_id: Option<&str>,
        next: Option<&str>,
    ) -> Result<(Vec<SearchAsset>, Option<String>)> {
        let mut body = serde_json::json!({
            "size": 1000,
            "withExif": true,
            "withDeleted": false,
            "page": next.and_then(|n| n.parse::<u32>().ok()).unwrap_or(1),
        });
        if let Some(id) = album_id {
            body["albumIds"] = serde_json::json!([id]);
        }
        let req = self
            .http
            .post(self.url("/search/metadata"))
            .header("x-api-key", &self.key)
            .json(&body);
        let page: SearchPage = Self::json(req).await?;
        let items = page
            .assets
            .items
            .into_iter()
            .map(|a| {
                let ex = a.exif_info.unwrap_or_default();
                SearchAsset {
                    id: a.id,
                    name: a.original_file_name,
                    is_image: a.kind.eq_ignore_ascii_case("IMAGE"),
                    checksum_hex: base64_to_hex(&a.checksum),
                    size: ex.file_size_in_byte.unwrap_or(0),
                    width: ex.exif_image_width,
                    height: ex.exif_image_height,
                    created_unix: crate::time::unix_from_rfc3339(&a.file_created_at).unwrap_or(0),
                    file_created_at: a.file_created_at,
                }
            })
            .collect();
        Ok((items, page.assets.next_page))
    }

    /// Wirft Assets in Immichs Papierkorb (wiederherstellbar, `force: false`).
    pub async fn trash_assets(&self, asset_ids: &[String]) -> Result<()> {
        let req = self
            .http
            .delete(self.url("/assets"))
            .header("x-api-key", &self.key)
            .json(&serde_json::json!({ "ids": asset_ids, "force": false }));
        Self::check(req.send().await?).await?;
        Ok(())
    }

    pub async fn add_to_album(
        &self,
        album_id: &str,
        asset_ids: &[String],
    ) -> Result<Vec<BulkIdResponse>> {
        let req = self
            .http
            .put(self.url(&format!("/albums/{album_id}/assets")))
            .header("x-api-key", &self.key)
            .json(&serde_json::json!({ "ids": asset_ids }));
        Self::json(req).await
    }

    /// Creates a share link for the assets and returns the address.
    /// If Immich has an external domain set, the link uses that,
    /// otherwise the server address from the settings.
    pub async fn share_link(&self, asset_ids: &[String]) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Link {
            key: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Config {
            #[serde(default)]
            external_domain: String,
        }
        let req = self
            .http
            .post(self.url("/shared-links"))
            .header("x-api-key", &self.key)
            .json(&serde_json::json!({
                "type": "INDIVIDUAL",
                "assetIds": asset_ids,
                "allowDownload": true,
                "showMetadata": true,
            }));
        let link: Link = Self::json(req).await?;
        let config: Config = Self::json(self.get("/server/config")).await?;
        let domain = config.external_domain.trim().trim_end_matches('/');
        let origin = if domain.is_empty() {
            self.base.trim_end_matches("/api")
        } else {
            domain
        };
        Ok(format!("{origin}/share/{}", link.key))
    }
}

async fn sha1_hex(path: &Path) -> Result<String> {
    let io = |e| Error::Io {
        path: path.to_owned(),
        source: e,
    };
    let mut file = tokio::fs::File::open(path).await.map_err(io)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).await.map_err(io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_url_has_no_key() {
        let c = Client::new("http://immich.example/", "geheim").unwrap();
        assert_eq!(
            c.playback_url("x1"),
            "http://immich.example/api/assets/x1/video/playback"
        );
        assert_eq!(c.api_key(), "geheim");
    }

    #[test]
    fn base_url_is_normalized() {
        for s in [
            "http://immich.example",
            "http://immich.example/",
            "http://immich.example/api",
            " http://immich.example/api/ ",
        ] {
            assert_eq!(Client::new(s, "k").unwrap().base, "http://immich.example/api");
        }
        assert!(matches!(
            Client::new("immich.example", "k"),
            Err(Error::BadUrl(_))
        ));
    }

    #[tokio::test]
    async fn sha1_matches_known_value() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha1_hex(&p).await.unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }
}

/// Immich delivers SHA-1 as Base64; hex is what sha1sum and friends print.
fn base64_to_hex(b64: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut acc, mut bits, mut out) = (0u32, 0u32, String::new());
    for c in b64.bytes().filter(|c| *c != b'=') {
        let Some(v) = T.iter().position(|t| *t == c) else {
            return String::new();
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push_str(&format!("{:02x}", (acc >> bits) & 0xff));
        }
    }
    out
}

#[cfg(test)]
mod b64_tests {
    #[test]
    fn base64_sha1_to_hex() {
        // sha1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        assert_eq!(
            super::base64_to_hex("qZk+NkcGgWq6PiVxeFDCbJzQ2J0="),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }
}
