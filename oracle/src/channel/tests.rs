use super::*;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use vault::daemon::HotkeyConfig;
use vault::frontmatter::Frontmatter;
use vault::note::Note;

async fn stub(status: &str, body: &str) -> (u16, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.expect("read");
        sock.write_all(response.as_bytes()).await.expect("write");
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    (port, handle)
}

fn view_for(port: u16) -> BorgView {
    BorgView {
        hotkey: HotkeyConfig {
            host: "127.0.0.1".to_string(),
            port,
            ..HotkeyConfig::default()
        },
        ..BorgView::default()
    }
}

const OK_BODY: &str = r#"{"handle":"@letsgetrusty","query":"tokio","hits":[
 {"id":"FUg1y-yv6cs","title":"How Rust engineered the perfect async runtime","url":"https://www.youtube.com/watch?v=FUg1y-yv6cs","duration-secs":488,"view-count":68792},
 {"id":"wXtngLBkK4Q","title":"Async Rust explained","url":"https://www.youtube.com/watch?v=wXtngLBkK4Q"}]}"#;

#[tokio::test]
async fn fetch_sends_handle_query_and_limit() {
    let (port, req) = stub("200 OK", OK_BODY).await;
    let s = fetch(&view_for(port), "@letsgetrusty", "tokio rt", Some(5))
        .await
        .expect("fetch");
    assert_eq!(s.hits.len(), 2);
    let line = req.await.unwrap();
    let first = line.lines().next().unwrap();
    assert!(first.starts_with("GET /channels/search?"), "{first}");
    assert!(first.contains("handle=%40letsgetrusty"), "{first}");
    assert!(
        first.contains("q=tokio+rt") || first.contains("q=tokio%20rt"),
        "{first}"
    );
    assert!(first.contains("limit=5"), "{first}");
}

#[tokio::test]
async fn closed_port_is_an_error_naming_the_address() {
    let closed = vault::testnet::closed_port();
    let port = closed.port();
    let msg = fetch(&view_for(port), "@x", "q", None).await.unwrap_err().to_string();
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

#[tokio::test]
async fn a_502_surfaces_the_daemon_error_and_stderr_tail_with_the_address() {
    let (port, _r) = stub(
        "502 Bad Gateway",
        r#"{"error":"yt-dlp exited 1","stderr-tail":"ERROR: channel gone"}"#,
    )
    .await;
    let msg = fetch(&view_for(port), "@x", "q", None).await.unwrap_err().to_string();
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
    assert!(msg.contains("yt-dlp exited 1"), "{msg}");
    assert!(msg.contains("ERROR: channel gone"), "{msg}");
}

#[tokio::test]
async fn a_400_names_the_address_and_error() {
    let (port, _r) = stub("400 Bad Request", r#"{"error":"invalid handle","stderr-tail":null}"#).await;
    let msg = fetch(&view_for(port), "bad", "q", None).await.unwrap_err().to_string();
    assert!(msg.contains("invalid handle"), "{msg}");
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

#[tokio::test]
async fn a_404_says_daemon_predates_the_route() {
    let (port, _r) = stub("404 Not Found", "").await;
    let msg = fetch(&view_for(port), "@x", "q", None).await.unwrap_err().to_string();
    assert!(msg.contains("predates /channels/search"), "{msg}");
}

#[tokio::test]
async fn garbage_200_body_is_an_error_naming_the_address() {
    let (port, _r) = stub("200 OK", "not json").await;
    let msg = fetch(&view_for(port), "@x", "q", None).await.unwrap_err().to_string();
    assert!(msg.contains("unparseable"), "{msg}");
    assert!(msg.contains(&format!("127.0.0.1:{port}")), "{msg}");
}

fn youtube_note(path: &str, source: &str) -> Note {
    Note {
        path: PathBuf::from(path),
        frontmatter: Frontmatter {
            title: Some(path.to_string()),
            note_type: Some("youtube".to_string()),
            origin: Some("assisted".to_string()),
            source: Some(source.to_string()),
            ..Frontmatter::default()
        },
        body: "body".to_string(),
        raw: "---\n---\nbody".to_string(),
    }
}

#[test]
fn render_marks_only_hits_the_vault_already_has() {
    let db = SearchIndex::open_memory().expect("db");
    // Different URL spelling than the hit's: the match is on the video id.
    db.index_one(&youtube_note("notes/rt.md", "https://youtu.be/FUg1y-yv6cs?t=30"), 1)
        .expect("index");
    let search: ChannelSearch = serde_json::from_str(OK_BODY).unwrap();
    let v = render(&search, &db).expect("render");
    assert_eq!(v["count"], 2);
    assert_eq!(v["results"][0]["in-vault"], "notes/rt.md");
    assert!(v["results"][1].get("in-vault").is_none());
    assert_eq!(v["results"][0]["duration-secs"], 488);
}

#[test]
fn render_ignores_non_youtube_notes_that_mention_the_id() {
    let db = SearchIndex::open_memory().expect("db");
    let mut n = youtube_note("notes/art.md", "https://example.com/?v=FUg1y-yv6cs");
    n.frontmatter.note_type = Some("article".to_string());
    db.index_one(&n, 1).expect("index");
    let search: ChannelSearch = serde_json::from_str(OK_BODY).unwrap();
    let v = render(&search, &db).expect("render");
    assert!(v["results"][0].get("in-vault").is_none());
}
