//! End-to-end discovery over a mock session tree, plus state wiring.

use mindplayer_core::{
    refresh_activity_and_usage, scan, Agent, Aggregate, ScanConfig, Scope, Session, State,
    TokenUsage,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

const CODEX_ID: &str = "11111111-2222-7333-8444-555566667777";
const CLAUDE_ID: &str = "99999999-8888-7777-6666-555544443333";
const KIRO_ID: &str = "44444444-4444-7444-8444-444444444444";

fn write(path: &Path, lines: &[&str]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, lines.join("\n")).unwrap();
}

/// Build a fake Codex + Claude store and return a ScanConfig pointing at it.
fn fixture() -> (tempfile::TempDir, ScanConfig) {
    let dir = tempdir().unwrap();
    let codex_dir = dir.path().join("codex");
    let claude_dir = dir.path().join("claude");

    // Codex session in /work, 1,000 total tokens.
    write(
        &codex_dir
            .join("2026/01/02")
            .join(format!("rollout-2026-01-02T20-09-08-{CODEX_ID}.jsonl")),
        &[
            &format!(
                r#"{{"timestamp":"2026-01-02T11:09:19.113Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","timestamp":"2026-01-02T11:09:08.122Z","cwd":"/work"}}}}"#
            ),
            r#"{"timestamp":"2026-01-02T11:10:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<x>tag</x> build mindplayer"}]}}"#,
            r#"{"timestamp":"2026-01-02T11:11:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":600,"cached_input_tokens":100,"output_tokens":400,"total_tokens":1000}}}}"#,
        ],
    );

    // Codex session in a different cwd (/other), 50 total tokens.
    write(
        &codex_dir.join(
            "2026/01/02/rollout-2026-01-02T09-00-00-aaaaaaaa-409a-72a1-8ab7-000000000001.jsonl",
        ),
        &[
            r#"{"type":"session_meta","payload":{"id":"aaaaaaaa-409a-72a1-8ab7-000000000001","timestamp":"2026-01-02T09:00:00Z","cwd":"/other"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"cached_input_tokens":0,"output_tokens":0,"total_tokens":50}}}}"#,
        ],
    );

    // Claude session in /work, usage 2+1080+2209+42463 across two messages.
    write(
        &claude_dir.join("-work").join(format!("{CLAUDE_ID}.jsonl")),
        &[
            &format!(
                r#"{{"type":"user","timestamp":"2026-01-03T02:04:41.130Z","cwd":"/work","sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":"hello claude"}}}}"#
            ),
            r#"{"type":"assistant","timestamp":"2026-01-03T02:05:00Z","message":{"role":"assistant","usage":{"input_tokens":2,"output_tokens":1080,"cache_creation_input_tokens":2209,"cache_read_input_tokens":42463}}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-03T02:06:00Z","message":{"role":"assistant","usage":{"input_tokens":2,"output_tokens":1080,"cache_creation_input_tokens":2209,"cache_read_input_tokens":42463}}}"#,
        ],
    );

    // Kiro CLI session in /work: a `<uuid>.json` metadata sidecar under cli/.
    let kiro_dir = dir.path().join("kiro");
    write(
        &kiro_dir.join("cli").join(format!("{KIRO_ID}.json")),
        &[&format!(
            r#"{{"session_id":"{KIRO_ID}","cwd":"/work","created_at":"2026-01-04T08:00:00Z","updated_at":"2026-01-04T08:05:00Z","title":"ship the release","session_created_reason":"user","session_state":{{"rts_model_state":{{"context_usage_percentage":42.5}}}}}}"#
        )],
    );
    // A second kiro session with reason "subagent" + null title in a different
    // cwd — proves the unreliable reason field doesn't hide it and the title
    // falls back.
    write(
        &kiro_dir
            .join("cli")
            .join("aaaa1111-0000-0000-0000-000000000000.json"),
        &[
            r#"{"session_id":"aaaa1111-0000-0000-0000-000000000000","cwd":"/other","created_at":"2026-01-04T07:00:00Z","updated_at":"2026-01-04T07:01:00Z","title":null,"session_created_reason":"subagent"}"#,
        ],
    );
    // A third kiro session: a fan-out worker whose title matches no known
    // prompt-prefix heuristic (a made-up template, not "You are "/"Read-only"/
    // etc.) — only `parent_session_id` marks it. Proves the structural signal
    // catches fan-outs the text heuristic has never seen, instead of needing a
    // new prefix added every time a new workflow's phrasing shows up.
    write(
        &kiro_dir
            .join("cli")
            .join("bbbb2222-0000-0000-0000-000000000000.json"),
        &[
            r#"{"session_id":"bbbb2222-0000-0000-0000-000000000000","cwd":"/other","created_at":"2026-01-04T07:10:00Z","updated_at":"2026-01-04T07:11:00Z","title":"summarize the notes in /work/scratch","session_created_reason":"subagent","parent_session_id":"aaaa1111-0000-0000-0000-000000000000"}"#,
        ],
    );

    (
        dir,
        ScanConfig {
            codex_dir,
            claude_dir,
            kiro_dir,
        },
    )
}

#[test]
fn global_scope_finds_all_three() {
    let (_d, cfg) = fixture();
    let sessions = scan(&Scope::Global, &cfg);
    assert_eq!(sessions.len(), 6, "expected 2 codex + 1 claude + 3 kiro");
}

#[test]
fn kiro_sessions_parsed_from_sidecar() {
    let (_d, cfg) = fixture();
    let sessions = scan(&Scope::Global, &cfg);
    let kiro: Vec<_> = sessions.iter().filter(|s| s.id == KIRO_ID).collect();
    assert_eq!(kiro.len(), 1);
    let s = kiro[0];
    assert_eq!(s.title, "ship the release");
    assert!(!s.is_subagent);
    assert!(s.started_at.is_some() && s.last_active.is_some());
    // kiro reports context-window occupancy instead of token totals.
    assert_eq!(s.context_pct, Some(42.5));
    assert_eq!(s.tokens.total, 0);

    // kiro-cli writes session_created_reason="subagent" even for normal
    // interactive sessions, so it must NOT be treated as a sub-agent (else real
    // sessions get hidden). A null title still falls back to a generic one.
    let other = sessions
        .iter()
        .find(|s| s.id == "aaaa1111-0000-0000-0000-000000000000")
        .expect("second kiro session discovered");
    assert!(
        !other.is_subagent,
        "kiro reason field must not hide sessions"
    );
    assert_eq!(other.title, "(kiro session)");
    assert_eq!(other.context_pct, None, "no context % when not reported");
}

#[test]
fn kiro_fan_out_worker_is_hidden_by_parent_session_id_even_with_a_novel_title() {
    let (_d, cfg) = fixture();
    let sessions = scan(&Scope::Global, &cfg);
    let worker = sessions
        .iter()
        .find(|s| s.id == "bbbb2222-0000-0000-0000-000000000000")
        .expect("fan-out worker session discovered");
    assert!(
        worker.is_subagent,
        "a non-empty parent_session_id must mark a session as a sub-agent \
         even when its title matches no known prompt-prefix heuristic"
    );
}

#[test]
fn working_dir_scope_filters_by_cwd() {
    let (_d, cfg) = fixture();
    let sessions = scan(&Scope::WorkingDir("/work".into()), &cfg);
    // codex + claude + kiro are in /work; the /other codex & kiro are excluded.
    assert_eq!(sessions.len(), 3);
    assert!(sessions.iter().all(|s| s.cwd == Path::new("/work")));
}

#[test]
fn tokens_are_parsed_per_agent() {
    let (_d, cfg) = fixture();
    let sessions = scan(&Scope::Global, &cfg);
    let agg = Aggregate::of(&sessions);
    assert_eq!(agg.codex.total, 1050, "1000 + 50 codex total");
    let per_msg = 2 + 1080 + 2209 + 42463;
    assert_eq!(agg.claude.total, (per_msg * 2) as u64);
    assert_eq!(agg.total.total, 1050 + (per_msg * 2) as u64);
    assert_eq!(agg.codex_count, 2);
    assert_eq!(agg.claude_count, 1);
}

#[test]
fn title_is_cleaned() {
    let (_d, cfg) = fixture();
    let sessions = scan(&Scope::Global, &cfg);
    let target = sessions.iter().find(|s| s.id == CODEX_ID).unwrap();
    assert_eq!(target.title, "tag build mindplayer");
}

#[test]
fn refresh_activity_populates_mtime_and_sorts() {
    use mindplayer_core::{Agent, Session};
    let dir = tempdir().unwrap();
    let mut sessions: Vec<Session> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            let file = dir.path().join(format!("{name}.jsonl"));
            fs::write(&file, "x").unwrap();
            Session {
                id: (*name).into(),
                agent: Agent::Codex,
                cwd: std::path::PathBuf::new(),
                file,
                started_at: None,
                last_active: None,
                last_prompt_at: None,
                tokens: Default::default(),
                title: String::new(),
                archived: false,
                is_subagent: false,
                context_pct: None,
            }
        })
        .collect();

    mindplayer_core::refresh_activity(&mut sessions);

    assert!(
        sessions.iter().all(|s| s.last_active.is_some()),
        "mtime populated for every session"
    );
    let times: Vec<_> = sessions.iter().map(|s| s.last_active.unwrap()).collect();
    assert!(
        times.windows(2).all(|w| w[0] >= w[1]),
        "refreshed sessions sorted newest-active first"
    );
}

#[test]
fn codex_tail_finds_token_after_huge_line() {
    // A single >256KB line before the token_count forces the tail window to
    // start mid-line; the seek-to-newline must not drop the real token record.
    let dir = tempdir().unwrap();
    let codex_dir = dir.path().join("codex");
    let claude_dir = dir.path().join("claude");
    let big_text = "x".repeat(400_000);
    write(
        &codex_dir.join(
            "2026/01/02/rollout-2026-01-02T10-00-00-bbbbbbbb-409a-72a1-8ab7-000000000002.jsonl",
        ),
        &[
            r#"{"type":"session_meta","payload":{"id":"bbbbbbbb-409a-72a1-8ab7-000000000002","timestamp":"2026-01-02T10:00:00Z","cwd":"/work"}}"#,
            &format!(
                r#"{{"type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{big_text}"}}]}}}}"#
            ),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":7,"cached_input_tokens":0,"output_tokens":3,"total_tokens":10}}}}"#,
        ],
    );
    let cfg = ScanConfig {
        codex_dir,
        claude_dir,
        kiro_dir: dir.path().join("kiro"),
    };
    let sessions = scan(&Scope::WorkingDir("/work".into()), &cfg);
    let s = sessions
        .iter()
        .find(|s| s.id == "bbbbbbbb-409a-72a1-8ab7-000000000002")
        .expect("session found");
    assert_eq!(
        s.tokens.total, 10,
        "token_count after huge line must be read"
    );
}

#[test]
fn refresh_activity_updates_usage_for_existing_rows() {
    let dir = tempdir().unwrap();
    let codex = dir.path().join("codex.jsonl");
    write(
        &codex,
        &[
            r#"{"type":"session_meta","payload":{"id":"codex-1","timestamp":"2026-01-02T10:00:00Z","cwd":"/work"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":7,"cached_input_tokens":2,"output_tokens":3,"total_tokens":10}}}}"#,
        ],
    );
    let claude = dir.path().join("claude.jsonl");
    write(
        &claude,
        &[
            r#"{"type":"last-prompt","sessionId":"claude-1"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-03T02:05:00Z","sessionId":"claude-1","cwd":"/work","message":{"role":"assistant","usage":{"input_tokens":2,"output_tokens":5,"cache_creation_input_tokens":11,"cache_read_input_tokens":13}}}"#,
        ],
    );

    let mut sessions = vec![
        Session {
            id: "codex-1".into(),
            agent: Agent::Codex,
            cwd: "/work".into(),
            file: codex,
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: TokenUsage::default(),
            title: "codex".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        },
        Session {
            id: "claude-1".into(),
            agent: Agent::Claude,
            cwd: "/work".into(),
            file: claude,
            started_at: None,
            last_active: None,
            last_prompt_at: None,
            tokens: TokenUsage::default(),
            title: "claude".into(),
            archived: false,
            is_subagent: false,
            context_pct: None,
        },
    ];

    refresh_activity_and_usage(&mut sessions);

    let codex = sessions.iter().find(|s| s.id == "codex-1").unwrap();
    assert_eq!(codex.tokens.total, 10);
    assert_eq!(codex.tokens.cached, 2);
    let claude = sessions.iter().find(|s| s.id == "claude-1").unwrap();
    assert_eq!(claude.tokens.total, 31);
    assert_eq!(claude.tokens.cached, 24);
}

#[test]
fn archive_state_merges_into_sessions() {
    let (_d, cfg) = fixture();
    let mut sessions = scan(&Scope::Global, &cfg);
    let mut state = State::default();
    state.set_archived(CODEX_ID, true);
    state.apply(&mut sessions);
    let target = sessions.iter().find(|s| s.id == CODEX_ID).unwrap();
    assert!(target.archived);
    assert!(sessions
        .iter()
        .filter(|s| s.id != CODEX_ID)
        .all(|s| !s.archived));
}

#[test]
fn claude_subagent_transcript_gets_its_own_id_not_the_parents() {
    // Regression: a spawned subagent's transcript (`<session>/subagents/*.jsonl`)
    // records the PARENT conversation's `sessionId` on every line, not one of
    // its own. Trusting that field verbatim made every subagent collide with
    // its parent under one id — corrupting anything keyed by id, notably the
    // periodic mtime refresh (a stale subagent's timestamp could silently
    // stamp over the parent's genuinely fresh one).
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join("claude");
    const PARENT_ID: &str = "22222222-3333-4444-5555-666677778888";

    write(
        &claude_dir.join("-work").join(format!("{PARENT_ID}.jsonl")),
        &[&format!(
            r#"{{"type":"user","timestamp":"2026-02-01T09:00:00Z","cwd":"/work","sessionId":"{PARENT_ID}","message":{{"role":"user","content":"parent turn"}}}}"#
        )],
    );
    // The subagent file's OWN name is "agent-sub1"; its content nonetheless
    // claims the parent's sessionId, exactly like real Claude Code output.
    write(
        &claude_dir
            .join("-work")
            .join(PARENT_ID)
            .join("subagents")
            .join("agent-sub1.jsonl"),
        &[&format!(
            r#"{{"type":"user","timestamp":"2026-02-01T09:05:00Z","cwd":"/work","sessionId":"{PARENT_ID}","isSidechain":true,"message":{{"role":"user","content":"You are TEAM WORKER doing the subtask"}}}}"#
        )],
    );

    let cfg = ScanConfig {
        codex_dir: dir.path().join("codex"),
        claude_dir,
        kiro_dir: dir.path().join("kiro"),
    };
    let sessions = scan(&Scope::Global, &cfg);
    assert_eq!(
        sessions.len(),
        2,
        "the parent and its subagent must be two distinct sessions, not merged into one"
    );

    let parent = sessions.iter().find(|s| s.id == PARENT_ID);
    assert!(parent.is_some(), "the parent keeps its real sessionId");
    assert!(!parent.unwrap().is_subagent);

    let sub = sessions
        .iter()
        .find(|s| s.id != PARENT_ID)
        .expect("the subagent must not collide with the parent's id");
    assert_eq!(
        sub.id, "agent-sub1",
        "a subagent's id comes from its own unique filename, not the shared sessionId field"
    );
    assert!(sub.is_subagent);
}

#[test]
fn claude_agent_name_marks_an_externally_orchestrated_subagent_with_a_novel_title() {
    // A subagent spawned by an external multi-agent orchestrator (headlessly
    // invoking `claude` as its own process, not via Claude Code's own Task
    // tool) gets a completely separate, top-level-looking session file:
    // isSidechain is false throughout, and it has its own real sessionId — the
    // parent/subagent nesting the test above covers doesn't apply here. The
    // only structural signal is a non-empty `agentName` (kiro's equivalent is
    // `parent_session_id` — see the kiro regression test). Title deliberately
    // matches none of `looks_like_subagent_title`'s prefixes, to prove the
    // structural field — not another hardcoded string — is what catches it.
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join("claude");
    const ORCHESTRATED_ID: &str = "33333333-4444-5555-6666-777788889999";

    write(
        &claude_dir
            .join("-work")
            .join(format!("{ORCHESTRATED_ID}.jsonl")),
        &[&format!(
            r#"{{"type":"user","timestamp":"2026-02-02T09:00:00Z","cwd":"/work","sessionId":"{ORCHESTRATED_ID}","isSidechain":false,"agentName":"worker-a","teamName":"session-parent0001","message":{{"role":"user","content":"summarize the notes in /work/scratch"}}}}"#
        )],
    );

    let cfg = ScanConfig {
        codex_dir: dir.path().join("codex"),
        claude_dir,
        kiro_dir: dir.path().join("kiro"),
    };
    let sessions = scan(&Scope::Global, &cfg);
    let worker = sessions
        .iter()
        .find(|s| s.id == ORCHESTRATED_ID)
        .expect("orchestrated subagent session discovered");
    assert!(
        worker.is_subagent,
        "a non-empty agentName must mark a session as a sub-agent even when \
         isSidechain is false and its title matches no known prompt-prefix heuristic"
    );
}

#[test]
fn codex_last_prompt_at_ignores_the_tool_round_trip_that_follows_it() {
    let dir = tempdir().unwrap();
    let codex_dir = dir.path().join("codex");
    const ID: &str = "aaaaaaaa-1111-7222-8333-444455556666";
    write(
        &codex_dir
            .join("2026/03/01")
            .join(format!("rollout-2026-03-01T09-00-00-{ID}.jsonl")),
        &[
            &format!(
                r#"{{"timestamp":"2026-03-01T09:00:00Z","type":"session_meta","payload":{{"id":"{ID}","timestamp":"2026-03-01T09:00:00Z","cwd":"/work"}}}}"#
            ),
            r#"{"timestamp":"2026-03-01T09:01:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a health check endpoint"}]}}"#,
            // A tool call/output round-trip after the prompt must never look
            // like a fresher prompt — codex keeps these as their own
            // `function_call`/`function_call_output` item types, never a
            // `message`/`role":"user"` record.
            r#"{"timestamp":"2026-03-01T09:02:00Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}"#,
            r#"{"timestamp":"2026-03-01T09:02:05Z","type":"response_item","payload":{"type":"function_call_output","output":"ok"}}"#,
            r#"{"timestamp":"2026-03-01T09:02:10Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"total_tokens":2}}}}"#,
        ],
    );
    let cfg = ScanConfig {
        codex_dir,
        claude_dir: dir.path().join("claude"),
        kiro_dir: dir.path().join("kiro"),
    };
    let sessions = scan(&Scope::WorkingDir("/work".into()), &cfg);
    let s = sessions.iter().find(|s| s.id == ID).expect("session found");
    assert_eq!(
        s.last_prompt_at.map(|t| t.to_rfc3339()),
        Some("2026-03-01T09:01:00+00:00".to_string()),
        "last_prompt_at must be the user message's own timestamp, not the later tool round-trip"
    );
}

#[test]
fn claude_last_prompt_at_ignores_tool_result_feedback() {
    let dir = tempdir().unwrap();
    let claude_dir = dir.path().join("claude");
    const ID: &str = "bbbbbbbb-2222-7333-8444-555566667777";
    write(
        &claude_dir.join("-work").join(format!("{ID}.jsonl")),
        &[
            &format!(
                r#"{{"type":"user","timestamp":"2026-03-02T10:00:00Z","cwd":"/work","sessionId":"{ID}","message":{{"role":"user","content":"fix the failing deploy"}}}}"#
            ),
            &format!(
                r#"{{"type":"assistant","timestamp":"2026-03-02T10:00:05Z","sessionId":"{ID}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"tu_1","name":"Bash","input":{{}}}}]}}}}"#
            ),
            // The tool's own result comes back as its own `type: "user"`
            // record — same shape a real prompt would have — and is later
            // than the genuine prompt above. It must not win.
            &format!(
                r#"{{"type":"user","timestamp":"2026-03-02T10:00:10Z","cwd":"/work","sessionId":"{ID}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tu_1","content":"deploy log tail"}}]}}}}"#
            ),
        ],
    );
    let cfg = ScanConfig {
        codex_dir: dir.path().join("codex"),
        claude_dir,
        kiro_dir: dir.path().join("kiro"),
    };
    let sessions = scan(&Scope::WorkingDir("/work".into()), &cfg);
    let s = sessions.iter().find(|s| s.id == ID).expect("session found");
    assert_eq!(
        s.last_prompt_at.map(|t| t.to_rfc3339()),
        Some("2026-03-02T10:00:00+00:00".to_string()),
        "last_prompt_at must be the genuine prompt's timestamp, not the later tool_result feedback"
    );
}
