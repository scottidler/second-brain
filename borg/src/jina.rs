use eyre::{Context, Result};

pub async fn fetch_article_markdown(url: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let jina_url = format!("https://r.jina.ai/{url}");

    let response = client
        .get(&jina_url)
        .header("Accept", "text/markdown")
        .send()
        .await
        .context("Failed to reach Jina Reader")?;

    if !response.status().is_success() {
        eyre::bail!(
            "Jina Reader returned status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }

    response
        .text()
        .await
        .context("Failed to read Jina Reader response body")
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_jina_url_format() {
        let url = "https://blog.example.com/post";
        let jina_url = format!("https://r.jina.ai/{url}");
        assert_eq!(jina_url, "https://r.jina.ai/https://blog.example.com/post");
    }
}
