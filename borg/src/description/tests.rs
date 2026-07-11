use super::*;

// ── extract_hashtags tests ──

#[test]
fn test_extract_hashtags_empty() {
    assert!(extract_hashtags("").is_empty());
}

#[test]
fn test_extract_hashtags_no_hashtags() {
    assert!(extract_hashtags("Just a normal description with no tags").is_empty());
}

#[test]
fn test_extract_hashtags_basic() {
    let tags = extract_hashtags("Check out #Rust and #programming tips");
    assert_eq!(tags, vec!["rust", "programming"]);
}

#[test]
fn test_extract_hashtags_with_hyphens() {
    let tags = extract_hashtags("#claude-code #home-lab #k8s");
    assert_eq!(tags, vec!["claude-code", "home-lab", "k8s"]);
}

#[test]
fn test_extract_hashtags_dedup() {
    let tags = extract_hashtags("#Rust #rust #RUST");
    assert_eq!(tags, vec!["rust"]);
}

#[test]
fn test_extract_hashtags_inline() {
    let tags = extract_hashtags("My #HomeLab tour after 3 years #Kubernetes #homelab");
    assert_eq!(tags, vec!["homelab", "kubernetes"]);
}

// ── filter_description tests ──

#[test]
fn test_filter_empty() {
    assert_eq!(filter_description(""), None);
}

#[test]
fn test_filter_whitespace_only() {
    assert_eq!(filter_description("   \n\n   "), None);
}

#[test]
fn test_filter_keeps_opening_paragraph() {
    let desc = "Kubernetes homelab tour after 3 years";
    assert_eq!(filter_description(desc), Some(desc.to_string()));
}

#[test]
fn test_filter_section_killer_kills_to_end() {
    let desc = "Recorded live on twitch, GET IN\n\
                     \n\
                     https://twitch.tv/ThePrimeagen\n\
                     \n\
                     MY MAIN YT CHANNEL: Has well edited engineering videos\n\
                     https://youtube.com/ThePrimeagen\n\
                     \n\
                     Discord\n\
                     https://discord.gg/ThePrimeagen";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(filtered.contains("Recorded live on twitch"));
    assert!(filtered.contains("twitch.tv/ThePrimeagen"));
    assert!(!filtered.contains("MY MAIN YT CHANNEL"));
    assert!(!filtered.contains("Discord"));
}

#[test]
fn test_filter_line_killers() {
    let desc = "Great video about Rust\n\
                     \n\
                     https://boot.dev/?promo=PRIMEYT\n\
                     \n\
                     Subscribe for more content!\n\
                     \n\
                     Reviewed video: https://youtube.com/watch?v=abc";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(filtered.contains("Great video about Rust"));
    assert!(!filtered.contains("promo="));
    assert!(!filtered.contains("Subscribe for more"));
    assert!(filtered.contains("Reviewed video"));
}

#[test]
fn test_filter_primeagen_example() {
    let desc = "Recorded live on twitch, GET IN\n\
                     \n\
                     https://twitch.tv/ThePrimeagen\n\
                     \n\
                     Become a backend engineer.  Its my favorite site\n\
                     https://boot.dev/?promo=PRIMEYT\n\
                     \n\
                     This is also the best way to support me is to support yourself becoming a better backend engineer.\n\
                     \n\
                     Reviewed video: https://www.youtube.com/watch?v=1UEMXDSh8Og\n\
                     By: https://www.youtube.com/@awesome-coding\n\
                     \n\
                     MY MAIN YT CHANNEL: Has well edited engineering videos\n\
                     https://youtube.com/ThePrimeagen\n\
                     \n\
                     Discord\n\
                     https://discord.gg/ThePrimeagen\n\
                     \n\
                     Have something for me to read or react to?: https://www.reddit.com/r/ThePrimeagenReact/\n\
                     \n\
                     Kinesis Advantage 360: https://bit.ly/Prime-Kinesis\n\
                     \n\
                     Hey I am sponsored by Turso, an edge database.\n\
                     https://turso.tech/deeznuts";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(filtered.contains("Recorded live on twitch, GET IN"));
    assert!(filtered.contains("twitch.tv/ThePrimeagen"));
    assert!(!filtered.contains("promo="));
    assert!(filtered.contains("Reviewed video"));
    assert!(filtered.contains("@awesome-coding"));
    assert!(!filtered.contains("MY MAIN YT CHANNEL"));
    assert!(!filtered.contains("Discord"));
    assert!(!filtered.contains("Kinesis"));
    assert!(!filtered.contains("Turso"));
}

#[test]
fn test_filter_strips_hashtags() {
    let desc = "My homelab tour #HomeLab #Kubernetes #selfhosted\n\nResources below";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(!filtered.contains('#'));
    assert!(filtered.contains("My homelab tour"));
    assert!(filtered.contains("Resources below"));
}

#[test]
fn test_filter_collapses_blank_lines() {
    let desc = "Line one\n\n\n\n\nLine two";
    let filtered = filter_description(desc).expect("should produce filtered output");
    // 2 blank lines = 3 newlines max between content lines; 4+ newlines means 3+ blanks
    assert!(
        !filtered.contains("\n\n\n\n"),
        "should have at most 2 consecutive blank lines"
    );
    assert!(filtered.contains("Line one"));
    assert!(filtered.contains("Line two"));
}

#[test]
fn test_filter_affiliate_line() {
    let desc = "Great tool for coding\n\
                     Use my affiliate link: https://example.com\n\
                     More content here";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(!filtered.contains("affiliate"));
    assert!(filtered.contains("Great tool"));
    assert!(filtered.contains("More content here"));
}

#[test]
fn test_filter_sponsored_line() {
    let desc = "Today's video\n\
                     Sponsored by NordVPN\n\
                     Back to the content";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(!filtered.contains("Sponsored by"));
    assert!(filtered.contains("Today's video"));
    assert!(filtered.contains("Back to the content"));
}

#[test]
fn test_filter_patreon_line() {
    let desc = "Good stuff\n\
                     Consider becoming a patron: https://patreon.com/creator\n\
                     More good stuff";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(!filtered.contains("patreon"));
    assert!(!filtered.contains("patron"));
}

#[test]
fn test_filter_lets_connect() {
    let desc = "Main content here\n\
                     \n\
                     Let's connect!\n\
                     Twitter: @handle\n\
                     Instagram: @handle";
    let filtered = filter_description(desc).expect("should produce filtered output");
    assert!(filtered.contains("Main content here"));
    assert!(!filtered.contains("connect"));
    assert!(!filtered.contains("Twitter"));
}

// ── extract_urls tests ──

#[test]
fn test_extract_urls_empty() {
    assert!(extract_urls("").is_empty());
}

#[test]
fn test_extract_urls_no_urls() {
    assert!(extract_urls("Just a normal description with no links at all.").is_empty());
}

/// Success criterion (a): a filtered description listing 10 tools returns
/// all 10 URLs, including the non-github `https://python.useinstructor.com/`.
/// This is a CONSTRUCTED fixture, not the real ht-4cbdf8 filtered
/// description -- the staged artifact only persists `distilled.yml`
/// (`links: []`), not the raw/filtered description text, so the real string
/// was unavailable. The fixture mirrors the real video's staged
/// `kind-specific.repos` list (9 github repos) plus Instructor's actual
/// non-github doc site, matching the concrete data-loss case cited in the
/// design doc. See implementation notes for detail.
#[test]
fn test_extract_urls_ht_4cbdf8_representative_fixture() {
    let desc = "Tools covered in this video:\n\
                10. Chunky: https://github.com/chonkie-ai/chonkie\n\
                9. Marker: https://github.com/VikParuchuri/marker\n\
                8. LangFuse: https://github.com/langfuse/langfuse\n\
                7. Qdrant: https://github.com/qdrant/qdrant\n\
                6. Ollama: https://github.com/ollama/ollama\n\
                5. DSPy: https://github.com/stanfordnlp/dspy\n\
                4. Crawl4AI: https://github.com/unclecode/crawl4ai\n\
                3. Outlines: https://github.com/dottxt-ai/outlines\n\
                2. LiteLLM: https://github.com/BerriAI/litellm\n\
                1. Instructor: https://python.useinstructor.com/";
    let links = extract_urls(desc);
    assert_eq!(links.len(), 10, "expected all 10 description urls, got {links:?}");
    let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
    assert!(
        urls.contains(&"https://python.useinstructor.com/"),
        "non-github url must survive: {urls:?}"
    );
    assert!(urls.contains(&"https://github.com/BerriAI/litellm"));
    // Sanity: the list-shape label rule fired for every line here.
    assert_eq!(links[0].label.as_deref(), Some("Chunky"));
    assert_eq!(
        links.last().expect("10 links extracted above").label.as_deref(),
        Some("Instructor")
    );
}

/// Success criterion (b): proves extraction ordering -- the seam must run
/// `filter_description` BEFORE `extract_urls`, not extract from the raw
/// description directly.
#[test]
fn test_extract_urls_ordering_requires_filter_first() {
    let raw = "Great video about tools\n\
               \n\
               Support me: https://patreon.com/foo\n\
               \n\
               More content here";

    // Sanity: the patreon url is really there in the raw text, so a missing
    // assertion below would be a false negative, not a tautology.
    let direct = extract_urls(raw);
    assert!(
        direct.iter().any(|l| l.url.contains("patreon")),
        "sanity check: raw description contains the url"
    );

    let filtered = filter_description(raw).expect("should produce filtered output");
    let links = extract_urls(&filtered);
    assert!(
        !links.iter().any(|l| l.url.contains("patreon")),
        "patreon url must be stripped by filter_description before extraction: {links:?}"
    );
}

/// Success criterion (c): urls differing only in path case are distinct
/// (dedup is EXACT, not case-insensitive like `extract_repo_slugs`).
#[test]
fn test_extract_urls_dedup_is_case_sensitive() {
    let desc = "https://example.com/Foo\nhttps://example.com/foo";
    let links = extract_urls(desc);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].url, "https://example.com/Foo");
    assert_eq!(links[1].url, "https://example.com/foo");
}

/// Break-the-code angle for (c): the exact same url repeated verbatim DOES
/// collapse to one entry, keeping the first-seen label.
#[test]
fn test_extract_urls_dedup_exact_duplicate_collapses() {
    let desc = "[First](https://example.com/foo)\nhttps://example.com/foo";
    let links = extract_urls(desc);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].url, "https://example.com/foo");
    assert_eq!(links[0].label.as_deref(), Some("First"));
}

/// Success criterion (d): the three label shapes.
#[test]
fn test_extract_urls_label_rules() {
    let desc = "[Instructor](https://useinstructor.com)\n\
                - LiteLLM: https://litellm.ai\n\
                https://example.com";
    let links = extract_urls(desc);
    assert_eq!(links.len(), 3);
    assert_eq!(links[0].url, "https://useinstructor.com");
    assert_eq!(links[0].label.as_deref(), Some("Instructor"));
    assert_eq!(links[1].url, "https://litellm.ai");
    assert_eq!(links[1].label.as_deref(), Some("LiteLLM"));
    assert_eq!(links[2].url, "https://example.com");
    assert_eq!(links[2].label, None);
}

/// Multiple urls on one line: per the Data Model rule, ALL become bare (the
/// label rule only applies when a url is the sole url on its line), even
/// when the line shape would otherwise look labelable.
#[test]
fn test_extract_urls_multiple_per_line_all_bare() {
    let desc = "Repo: https://github.com/a/b and also https://github.com/c/d";
    let links = extract_urls(desc);
    assert_eq!(links.len(), 2);
    assert!(
        links.iter().all(|l| l.label.is_none()),
        "multi-url lines must not derive a label: {links:?}"
    );
}

/// Success criterion (e): balanced-paren keep, unbalanced trim, scheme-less
/// drop.
#[test]
fn test_extract_urls_trim_and_scheme_rules() {
    let desc = "Check out https://en.wikipedia.org/wiki/Rust_(programming_language) for background.\n\
                see https://x.com/.\n\
                also see www.x.com for more";
    let links = extract_urls(desc);
    let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(
        urls,
        vec![
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "https://x.com/"
        ]
    );
}

// ── merge_description_links tests ──

#[test]
fn test_merge_description_links_empty() {
    let mut links: Vec<Link> = Vec::new();
    let (added, dropped) = merge_description_links(&mut links, "");
    assert_eq!((added, dropped), (0, 0));
    assert!(links.is_empty());
}

#[test]
fn test_merge_description_links_dedup_against_existing() {
    let mut links = vec![Link {
        url: "https://github.com/a/b".to_string(),
        label: Some("A/B".to_string()),
    }];
    let filtered = "See also https://github.com/a/b and https://github.com/c/d";

    let (added, dropped) = merge_description_links(&mut links, filtered);

    assert_eq!(added, 1);
    assert_eq!(dropped, 1);
    assert_eq!(links.len(), 2);
    // The pre-existing (LLM-emitted) link's label is untouched by the merge.
    assert_eq!(links[0].url, "https://github.com/a/b");
    assert_eq!(links[0].label.as_deref(), Some("A/B"));
    assert_eq!(links[1].url, "https://github.com/c/d");
}

/// Break-the-code angle: a description with only urls already present in
/// `links` adds nothing and drops everything found.
#[test]
fn test_merge_description_links_all_duplicates_adds_nothing() {
    let mut links = vec![Link {
        url: "https://example.com/foo".to_string(),
        label: None,
    }];
    let (added, dropped) = merge_description_links(&mut links, "https://example.com/foo");
    assert_eq!(added, 0);
    assert_eq!(dropped, 1);
    assert_eq!(links.len(), 1);
}
