use super::*;

fn vision(description: &str, text: &str) -> VisionResult {
    VisionResult {
        description: description.to_string(),
        suggested_title: String::new(),
        suggested_tags: Vec::new(),
        extracted_text: text.to_string(),
    }
}

#[test]
fn image_section_prefers_vision_text_under_extracted_text() {
    let v = vision("A diagram.", "vision words");
    let section = image_section(Some(&v), "ocr words", "##");
    assert_eq!(
        section,
        "## Description\n\nA diagram.\n\n## Extracted Text\n\nvision words"
    );
}

#[test]
fn image_section_falls_back_to_ocr_when_vision_has_no_text() {
    let v = vision("A photo.", "");
    let section = image_section(Some(&v), "ocr words", "####");
    assert_eq!(
        section,
        "#### Description\n\nA photo.\n\n#### Extracted Text\n\nocr words"
    );
}

#[test]
fn image_section_without_vision_is_ocr_text() {
    assert_eq!(image_section(None, "ocr words", "##"), "## OCR Text\n\nocr words");
}

#[test]
fn image_section_with_nothing_is_empty() {
    assert_eq!(image_section(None, "", "##"), "");
    assert_eq!(image_section(Some(&vision("", "")), "", "####"), "");
}
