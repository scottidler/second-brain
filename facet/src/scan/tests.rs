use std::fs;
use std::io::Write;

use super::*;

fn write_jsonl(path: &Path, body: &str) {
    let mut f = fs::File::create(path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
}

fn turn(uuid: &str, parent: Option<&str>, role: &str, ts: &str, text: &str, session: &str) -> String {
    turn_with_cwd(uuid, parent, role, ts, text, session, None)
}

fn turn_with_cwd(
    uuid: &str,
    parent: Option<&str>,
    role: &str,
    ts: &str,
    text: &str,
    session: &str,
    cwd: Option<&str>,
) -> String {
    let parent_field = match parent {
        Some(p) => format!("\"parentUuid\":\"{p}\","),
        None => "\"parentUuid\":null,".to_string(),
    };
    let role_field = if role == "assistant" {
        "\"role\":\"assistant\",\"model\":\"sonnet\""
    } else {
        "\"role\":\"user\""
    };
    let cwd_field = cwd.map(|c| format!("\"cwd\":\"{c}\",")).unwrap_or_default();
    format!(
        "{{\"type\":\"{role}\",\"uuid\":\"{uuid}\",{parent_field}\"timestamp\":\"{ts}\",{cwd_field}\"sessionId\":\"{session}\",\
         \"message\":{{{role_field},\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
    )
}

#[test]
fn decode_cwd_basic() {
    let p = std::path::PathBuf::from("/whatever/-home-saidler-repos-x");
    assert_eq!(decode_cwd(&p), std::path::PathBuf::from("/home/saidler/repos/x"));
}

#[test]
fn decode_cwd_unprefixed_passes_through() {
    let p = std::path::PathBuf::from("/whatever/oddname");
    assert_eq!(decode_cwd(&p), p);
}

#[test]
fn find_session_files_groups_parent_and_subagents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let projects = dir.path();
    let proj = projects.join("-home-me-x");
    fs::create_dir_all(&proj).expect("mk proj");
    let parent_id = "11111111-aaaa-4222-8333-cccccccccccc";
    let parent_path = proj.join(format!("{parent_id}.jsonl"));
    write_jsonl(
        &parent_path,
        &turn("u1", None, "user", "2026-05-26T00:00:00Z", "p", parent_id),
    );
    let sub_dir = proj.join(parent_id).join("subagents");
    fs::create_dir_all(&sub_dir).expect("mk sub");
    let sub_id = "44444444-bbbb-4555-8666-dddddddddddd";
    write_jsonl(
        &sub_dir.join(format!("{sub_id}.jsonl")),
        &turn("u2", None, "user", "2026-05-26T00:00:01Z", "s", sub_id),
    );

    let files = find_session_files(projects).expect("find");
    assert_eq!(files.len(), 2);
    let parents: Vec<_> = files.iter().filter(|f| f.kind == SessionFileKind::Parent).collect();
    let subs: Vec<_> = files.iter().filter(|f| f.kind == SessionFileKind::Subagent).collect();
    assert_eq!(parents.len(), 1);
    assert_eq!(subs.len(), 1);
    assert_eq!(parents[0].group_id, subs[0].group_id);
}

#[test]
fn find_session_files_skips_empty_jsonl() {
    let dir = tempfile::tempdir().expect("tempdir");
    let projects = dir.path();
    let proj = projects.join("-x");
    fs::create_dir_all(&proj).expect("mk");
    let empty = proj.join("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee.jsonl");
    fs::File::create(&empty).expect("touch");
    let files = find_session_files(projects).expect("find");
    assert!(files.is_empty());
}

#[test]
fn enumerate_filters_excluded_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let projects = dir.path().join("projects");
    let excluded_proj = projects.join("-home-me-tatari-tv-secret");
    let kept_proj = projects.join("-home-me-scottidler-x");
    fs::create_dir_all(&excluded_proj).expect("mk e");
    fs::create_dir_all(&kept_proj).expect("mk k");
    let id_a = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let id_b = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    write_jsonl(
        &excluded_proj.join(format!("{id_a}.jsonl")),
        &turn_with_cwd(
            "u1",
            None,
            "user",
            "2026-05-26T00:00:00Z",
            "excluded",
            id_a,
            Some("/home/me/tatari-tv/secret"),
        ),
    );
    write_jsonl(
        &kept_proj.join(format!("{id_b}.jsonl")),
        &turn_with_cwd(
            "u1",
            None,
            "user",
            "2026-05-26T00:00:00Z",
            "kept",
            id_b,
            Some("/home/me/scottidler/x"),
        ),
    );

    let cfg = crate::config::Config {
        claude_projects_root: projects.clone(),
        include_cwds: vec![],
        exclude_cwds: vec![std::path::PathBuf::from("/home/me/tatari-tv")],
        ..Default::default()
    };

    let ledger = Ledger::open_in_memory().expect("ledger");
    let sessions = enumerate(&cfg, &ledger).expect("enum");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_uuid, id_b);
}

#[test]
fn enumerate_uses_ledger_byte_offset_to_skip_seen_turns() {
    use crate::ledger::sessions::UpsertSession;
    let dir = tempfile::tempdir().expect("tempdir");
    let projects = dir.path().join("projects");
    let proj = projects.join("-home-me-scottidler-x");
    fs::create_dir_all(&proj).expect("mk");
    let id_c = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let mut body = String::new();
    body.push_str(&turn("u1", None, "user", "2026-05-26T00:00:00Z", "first", id_c));
    let after_first = body.len() as u64;
    body.push_str(&turn(
        "u2",
        Some("u1"),
        "assistant",
        "2026-05-26T00:00:01Z",
        "second",
        id_c,
    ));
    write_jsonl(&proj.join(format!("{id_c}.jsonl")), &body);

    let cfg = crate::config::Config {
        claude_projects_root: projects.clone(),
        include_cwds: vec![],
        exclude_cwds: vec![],
        ..Default::default()
    };

    let ledger = Ledger::open_in_memory().expect("ledger");
    ledger
        .upsert_session(UpsertSession {
            session_uuid: id_c,
            cwd: "/home/me/scottidler/x",
            repo_slug: None,
            seen_at: chrono::Utc::now(),
        })
        .expect("upsert");
    ledger
        .set_cluster_offset(id_c, after_first, Some("u1"))
        .expect("offset");

    let sessions = enumerate(&cfg, &ledger).expect("enum");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].parsed.turns.len(), 1);
    assert_eq!(sessions[0].parsed.turns[0].uuid, "u2");
}

#[test]
fn enumerate_drops_sessions_with_no_new_turns() {
    use crate::ledger::sessions::UpsertSession;
    let dir = tempfile::tempdir().expect("tempdir");
    let projects = dir.path().join("projects");
    let proj = projects.join("-home-me-scottidler-x");
    fs::create_dir_all(&proj).expect("mk");
    let id_d = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let body = turn("u1", None, "user", "2026-05-26T00:00:00Z", "only", id_d);
    let full_len = body.len() as u64;
    write_jsonl(&proj.join(format!("{id_d}.jsonl")), &body);

    let cfg = crate::config::Config {
        claude_projects_root: projects.clone(),
        include_cwds: vec![],
        exclude_cwds: vec![],
        ..Default::default()
    };

    let ledger = Ledger::open_in_memory().expect("ledger");
    ledger
        .upsert_session(UpsertSession {
            session_uuid: id_d,
            cwd: "/home/me/scottidler/x",
            repo_slug: None,
            seen_at: chrono::Utc::now(),
        })
        .expect("upsert");
    ledger.set_cluster_offset(id_d, full_len, Some("u1")).expect("offset");

    let sessions = enumerate(&cfg, &ledger).expect("enum");
    assert!(sessions.is_empty(), "no new turns, session should be dropped");
}
