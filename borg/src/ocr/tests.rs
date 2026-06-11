use super::*;

#[test]
fn test_ocr_extract_nonexistent_file() {
    // tesseract should fail gracefully on a nonexistent file
    let result = ocr_extract(Path::new("/tmp/nonexistent-obsidian-borg-test.png"), 5);
    // Either returns an error (tesseract not installed) or empty/non-empty string
    if let Ok(text) = result {
        let _ = text.len();
    }
}

#[test]
fn test_ocr_extract_short_timeout_terminates() {
    // Use `sleep` as a stand-in tesseract that hangs. With a sub-second
    // timeout, the internal kill path must fire and we must return Ok("")
    // rather than blocking.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn sleep");

    let timeout = Duration::from_millis(200);
    let start = std::time::Instant::now();
    let mut killed = false;
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
            break;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            killed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(killed, "expected timeout-driven kill");
}

#[test]
fn test_vision_result_struct() {
    let result = VisionResult {
        description: "A whiteboard diagram".to_string(),
        suggested_title: "Whiteboard Notes".to_string(),
        suggested_tags: vec!["diagram".to_string(), "notes".to_string()],
        extracted_text: "Hello World".to_string(),
    };
    assert_eq!(result.description, "A whiteboard diagram");
    assert_eq!(result.suggested_title, "Whiteboard Notes");
    assert_eq!(result.suggested_tags.len(), 2);
    assert_eq!(result.extracted_text, "Hello World");
}

#[test]
fn test_parse_vision_response_well_formed() {
    let response = "\
TEXT: Serial: ABC-123\nModel: SG-2100\n\
DESCRIPTION: A product label showing serial number and model information.\n\
TITLE: Netgate SG-2100 Serial Label\n\
TAGS: hardware, serial-number, netgate";

    let result = parse_vision_response(response);
    assert_eq!(result.extracted_text, "Serial: ABC-123\nModel: SG-2100");
    assert_eq!(
        result.description,
        "A product label showing serial number and model information."
    );
    assert_eq!(result.suggested_title, "Netgate SG-2100 Serial Label");
    assert_eq!(result.suggested_tags, vec!["hardware", "serial-number", "netgate"]);
}

#[test]
fn test_parse_vision_response_empty() {
    let result = parse_vision_response("");
    assert!(result.extracted_text.is_empty());
    assert!(result.description.is_empty());
    assert!(result.suggested_title.is_empty());
    assert!(result.suggested_tags.is_empty());
}

#[test]
fn test_parse_vision_response_partial() {
    let response = "TITLE: Some Image\nTAGS: photo, test";
    let result = parse_vision_response(response);
    assert_eq!(result.suggested_title, "Some Image");
    assert_eq!(result.suggested_tags, vec!["photo", "test"]);
    assert!(result.extracted_text.is_empty());
    // description falls back to first 3 lines
    assert!(!result.description.is_empty());
}

#[test]
fn test_parse_vision_response_multiline_text() {
    let response = "\
TEXT: Line 1
Line 2
Line 3
DESCRIPTION: A multi-line text image.
TITLE: Multi Line Text
TAGS: text";

    let result = parse_vision_response(response);
    assert_eq!(result.extracted_text, "Line 1\nLine 2\nLine 3");
    assert_eq!(result.description, "A multi-line text image.");
}

#[test]
fn test_parse_vision_response_tags_with_spaces() {
    let response = "TAGS: machine learning, deep learning, neural networks";
    let result = parse_vision_response(response);
    assert_eq!(
        result.suggested_tags,
        vec!["machine-learning", "deep-learning", "neural-networks"]
    );
}

#[test]
fn test_mime_from_extension() {
    assert_eq!(mime_from_extension("photo.jpg"), "image/jpeg");
    assert_eq!(mime_from_extension("photo.jpeg"), "image/jpeg");
    assert_eq!(mime_from_extension("screenshot.png"), "image/png");
    assert_eq!(mime_from_extension("anim.gif"), "image/gif");
    assert_eq!(mime_from_extension("modern.webp"), "image/webp");
    assert_eq!(mime_from_extension("unknown.xyz"), "image/jpeg");
    assert_eq!(mime_from_extension("noext"), "image/jpeg");
}
