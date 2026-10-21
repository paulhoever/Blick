//! Immich API responses, only the fields Blick needs.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub album_name: String,
    #[serde(default)]
    pub asset_count: u64,
    #[serde(default)]
    pub album_thumbnail_asset_id: Option<String>,
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeBucket {
    /// Start of the month as an ISO timestamp, exactly as the server delivers it.
    /// Passed back unchanged to `/timeline/bucket`.
    pub time_bucket: String,
    pub count: u64,
}

/// `/timeline/bucket` answers column-wise: one array per field.
/// That saves a lot of JSON on big albums but is awkward to work with.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BucketColumns {
    pub id: Vec<String>,
    pub is_image: Vec<bool>,
    pub ratio: Vec<f64>,
    pub file_created_at: Vec<String>,
    #[serde(default)]
    pub thumbhash: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteAsset {
    pub id: String,
    pub is_image: bool,
    /// Width over height.
    pub ratio: f64,
    pub file_created_at: String,
    pub thumbhash: Option<String>,
}

impl BucketColumns {
    pub(crate) fn into_assets(self) -> Vec<RemoteAsset> {
        let n = self.id.len();
        let mut thumbhash = self.thumbhash.into_iter();
        self.id
            .into_iter()
            .zip(self.is_image)
            .zip(self.ratio)
            .zip(self.file_created_at)
            .map(|(((id, is_image), ratio), file_created_at)| RemoteAsset {
                id,
                is_image,
                ratio,
                file_created_at,
                thumbhash: thumbhash.next().flatten(),
            })
            .take(n)
            .collect()
    }
}

/// A hit of the metadata search, with everything the duplicate search needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchAsset {
    pub id: String,
    pub name: String,
    pub is_image: bool,
    /// SHA-1 of the original, hex.
    pub checksum_hex: String,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub file_created_at: String,
    /// Unix seconds, from `file_created_at`; 0 if unreadable.
    pub created_unix: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchPage {
    pub assets: SearchAssets,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchAssets {
    pub items: Vec<RawSearchAsset>,
    #[serde(default)]
    pub next_page: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawSearchAsset {
    pub id: String,
    #[serde(default)]
    pub original_file_name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub checksum: String,
    #[serde(default)]
    pub file_created_at: String,
    #[serde(default)]
    pub exif_info: Option<RawExif>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawExif {
    pub file_size_in_byte: Option<u64>,
    pub exif_image_width: Option<u32>,
    pub exif_image_height: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadStatus {
    Created,
    Duplicate,
    /// Older servers still know `replaced`.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UploadResponse {
    pub id: String,
    pub status: UploadStatus,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkIdResponse {
    pub id: String,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl TimeBucket {
    /// Year and month (1–12) from the timestamp, without time zone math:
    /// Immich stores the start of the month in local time and just appends a Z.
    pub fn year_month(&self) -> Option<(i32, u32)> {
        let y = self.time_bucket.get(..4)?.parse().ok()?;
        let m: u32 = self.time_bucket.get(5..7)?.parse().ok()?;
        (1..=12).contains(&m).then_some((y, m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(t: &str) -> TimeBucket {
        TimeBucket {
            time_bucket: t.into(),
            count: 1,
        }
    }

    #[test]
    fn year_month_from_bucket() {
        assert_eq!(
            bucket("2026-10-01T00:00:00.000Z").year_month(),
            Some((2026, 10))
        );
        assert_eq!(bucket("1999-01-01").year_month(), Some((1999, 1)));
        assert_eq!(bucket("2026-13-01").year_month(), None);
        assert_eq!(bucket("kaputt").year_month(), None);
    }
}
