use blick_immich::{Client, Error, ThumbSize, UploadStatus};
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-key";

async fn setup() -> (MockServer, Client) {
    let server = MockServer::start().await;
    let client = Client::new(&server.uri(), KEY).unwrap();
    (server, client)
}

#[tokio::test]
async fn wrong_key_is_unauthorized() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/api/users/me"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"message": "Invalid API key", "statusCode": 401}),
            ),
        )
        .mount(&server)
        .await;
    assert!(matches!(client.me().await, Err(Error::Unauthorized)));
}

#[tokio::test]
async fn api_errors_carry_server_message() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/api/albums"))
        .respond_with(
            ResponseTemplate::new(500).set_body_json(serde_json::json!({"message": "kaputt"})),
        )
        .mount(&server)
        .await;
    match client.albums().await {
        Err(Error::Api { status, message }) => {
            assert_eq!((status, message.as_str()), (500, "kaputt"))
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn albums_are_sorted_and_key_is_sent() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/api/albums"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "2", "albumName": "urlaub", "assetCount": 3, "albumThumbnailAssetId": null, "shared": false},
            {"id": "1", "albumName": "Garten", "assetCount": 12, "albumThumbnailAssetId": "a1", "shared": true}
        ])))
        .mount(&server)
        .await;
    let albums = client.albums().await.unwrap();
    assert_eq!(
        albums.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["1", "2"]
    );
    assert_eq!(albums[0].album_thumbnail_asset_id.as_deref(), Some("a1"));
}

#[tokio::test]
async fn bucket_columns_become_assets() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/api/timeline/bucket"))
        .and(query_param("albumId", "alb"))
        .and(query_param("timeBucket", "2026-09-01T00:00:00.000Z"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": ["a", "b"], "isImage": [true, false], "ratio": [1.5, 0.75],
            "fileCreatedAt": ["2026-09-02T10:00:00.000Z", "2026-09-03T10:00:00.000Z"],
            "thumbhash": [null, "1QcSHQRnh493V4dIh4eXh1h4kJUI"], "ownerId": ["u", "u"]
        })))
        .mount(&server)
        .await;
    let assets = client
        .bucket_assets(Some("alb"), "2026-09-01T00:00:00.000Z")
        .await
        .unwrap();
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[1].id, "b");
    assert!(!assets[1].is_image);
    assert_eq!(assets[1].ratio, 0.75);
    assert!(assets[0].thumbhash.is_none() && assets[1].thumbhash.is_some());
}

#[tokio::test]
async fn thumbnail_asks_for_size() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/api/assets/x/thumbnail"))
        .and(query_param("size", "preview"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"jpegdata".to_vec()))
        .mount(&server)
        .await;
    assert_eq!(
        &client.thumbnail("x", ThumbSize::Preview).await.unwrap()[..],
        b"jpegdata"
    );
}

#[tokio::test]
async fn upload_sends_multipart_with_checksum() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/assets"))
        .and(header("x-api-key", KEY))
        .and(header(
            "x-immich-checksum",
            "a9993e364706816aba3e25717850c26c9cd0d89d",
        ))
        .and(header_exists("content-type"))
        .respond_with(|req: &Request| {
            let body = String::from_utf8_lossy(&req.body);
            let ok = [
                "name=\"assetData\"",
                "filename=\"foto.jpg\"",
                "name=\"fileCreatedAt\"",
                "name=\"fileModifiedAt\"",
                "abc",
            ]
            .iter()
            .all(|needle| body.contains(needle));
            if ok {
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"id": "new", "status": "created"}))
            } else {
                ResponseTemplate::new(400).set_body_string(body.into_owned())
            }
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("foto.jpg");
    std::fs::write(&file, b"abc").unwrap();
    let r = client.upload(&file).await.unwrap();
    assert_eq!((r.id.as_str(), r.status), ("new", UploadStatus::Created));
}

#[tokio::test]
async fn add_to_album_sends_ids() {
    let (server, client) = setup().await;
    Mock::given(method("PUT"))
        .and(path("/api/albums/alb/assets"))
        .and(body_json(serde_json::json!({"ids": ["a", "b"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "a", "success": true}, {"id": "b", "success": false, "error": "duplicate"}
        ])))
        .mount(&server)
        .await;
    let r = client
        .add_to_album("alb", &["a".into(), "b".into()])
        .await
        .unwrap();
    assert!(r[0].success && !r[1].success);
}

#[tokio::test]
async fn download_writes_file() {
    let (server, client) = setup().await;
    Mock::given(method("GET"))
        .and(path("/api/assets/x/original"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"originaldaten".to_vec()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("foto.heic");
    assert_eq!(client.download_original("x", &dst).await.unwrap(), 13);
    assert_eq!(std::fs::read(&dst).unwrap(), b"originaldaten");
}

#[tokio::test]
async fn share_link_for_assets_uses_server_address() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/shared-links"))
        .and(header("x-api-key", KEY))
        .and(body_json(serde_json::json!({
            "type": "INDIVIDUAL",
            "assetIds": ["a", "b"],
            "allowDownload": true,
            "showMetadata": true
        })))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": "l1", "key": "abc"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/server/config"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"externalDomain": ""})),
        )
        .mount(&server)
        .await;
    let url = client.share_link(&["a".into(), "b".into()]).await.unwrap();
    assert_eq!(url, format!("{}/share/abc", server.uri()));
}

#[tokio::test]
async fn share_link_prefers_external_domain() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/shared-links"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": "l1", "key": "abc"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/server/config"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"externalDomain": "https://fotos.example.org/"})),
        )
        .mount(&server)
        .await;
    let url = client.share_link(&["a".into()]).await.unwrap();
    assert_eq!(url, "https://fotos.example.org/share/abc");
}

#[tokio::test]
async fn search_page_returns_checksum_size_and_next_page() {
    let (server, client) = setup().await;
    Mock::given(method("POST"))
        .and(path("/api/search/metadata"))
        .and(header("x-api-key", KEY))
        .and(body_json(serde_json::json!({
            "size": 1000, "withExif": true, "withDeleted": false, "page": 1
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "assets": {
                "items": [{
                    "id": "a1", "originalFileName": "IMG_1.jpg", "type": "IMAGE",
                    "checksum": "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=",
                    "fileCreatedAt": "1970-01-02T00:00:00.000Z",
                    "exifInfo": {"fileSizeInByte": 1234, "exifImageWidth": 400, "exifImageHeight": 300}
                }],
                "nextPage": "2"
            }
        })))
        .mount(&server)
        .await;
    let (items, next) = client.search_page(None, None).await.unwrap();
    assert_eq!(next.as_deref(), Some("2"));
    let a = &items[0];
    assert_eq!(a.checksum_hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!((a.size, a.width, a.height, a.created_unix), (1234, Some(400), Some(300), 86_400));
    assert!(a.is_image);
}

#[tokio::test]
async fn trash_assets_is_not_forced() {
    let (server, client) = setup().await;
    Mock::given(method("DELETE"))
        .and(path("/api/assets"))
        .and(body_json(serde_json::json!({"ids": ["a1"], "force": false})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    client.trash_assets(&["a1".to_string()]).await.unwrap();
}
