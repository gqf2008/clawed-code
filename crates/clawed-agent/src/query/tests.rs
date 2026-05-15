//! Unit tests for the query stream loop and helpers.

use super::helpers::{
    block_to_api, build_context_warning, build_system_blocks, classify_api_error, error_category,
    make_continuation_message, messages_to_api, ApiErrorAction,
};
use super::*;

// ── classify_api_error ───────────────────────────────────────────────

#[test]
fn test_classify_prompt_too_long_triggers_compact() {
    let action = classify_api_error("prompt is too long", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::ReactiveCompact));
}

#[test]
fn test_classify_prompt_too_long_already_compacted() {
    let action = classify_api_error("prompt is too long", true, 0, 1000);
    assert!(matches!(action, ApiErrorAction::Fatal));
}

#[test]
fn test_classify_413_status() {
    let action = classify_api_error("HTTP 413 payload too large", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::ReactiveCompact));
}

#[test]
fn test_classify_too_many_tokens() {
    let action = classify_api_error("too many tokens in request", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::ReactiveCompact));
}

#[test]
fn test_classify_rate_limit_retryable() {
    let action = classify_api_error("rate limit exceeded", false, 1, 2000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 2000 }));
}

#[test]
fn test_classify_529_overloaded() {
    let action = classify_api_error("529 service overloaded", false, 2, 5000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 5000 }));
}

#[test]
fn test_classify_500_server_error() {
    let action = classify_api_error("500 internal server error", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 1000 }));
}

#[test]
fn test_classify_503_service_unavailable() {
    let action = classify_api_error("503 service unavailable", false, 3, 3000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 3000 }));
}

#[test]
fn test_classify_retry_after_header() {
    let action = classify_api_error("rate limited retry-after: 10", false, 1, 2000);
    assert!(matches!(action, ApiErrorAction::Retry { wait_ms: 10000 }));
}

#[test]
fn test_classify_max_consecutive_errors_exceeded() {
    let action = classify_api_error("rate limit", false, 6, 1000);
    assert!(matches!(action, ApiErrorAction::Fatal));
}

#[test]
fn test_classify_unknown_error_fatal() {
    let action = classify_api_error("something unexpected happened", false, 0, 1000);
    assert!(matches!(action, ApiErrorAction::Fatal));
}

// ── error_category ───────────────────────────────────────────────────

#[test]
fn test_error_category_rate_limit() {
    assert_eq!(error_category("rate limit exceeded"), "rate_limit");
    assert_eq!(error_category("429 too many requests"), "rate_limit");
}

#[test]
fn test_error_category_overloaded() {
    assert_eq!(error_category("overloaded"), "overloaded");
    assert_eq!(error_category("529 overloaded"), "overloaded");
}

#[test]
fn test_error_category_server_error() {
    assert_eq!(error_category("500 internal"), "server_error");
    assert_eq!(error_category("503 unavailable"), "server_error");
}

#[test]
fn test_error_category_generic() {
    assert_eq!(error_category("something else entirely"), "api_error");
}

// ── build_context_warning ────────────────────────────────────────────

const TEST_CONTEXT_WINDOW: u64 = 200_000;

#[test]
fn test_build_context_warning_normal() {
    // 40% of dynamic threshold (~167K * 0.4 = ~67K) should be normal
    assert!(build_context_warning(60_000, TEST_CONTEXT_WINDOW).is_none());
}

#[test]
fn test_build_context_warning_warning_level() {
    let threshold = crate::compact::get_auto_compact_threshold(TEST_CONTEXT_WINDOW);
    let at_60 = (threshold as f64 * 0.60) as u64;
    let result = build_context_warning(at_60, TEST_CONTEXT_WINDOW);
    assert!(result.is_some());
    if let Some((
        level,
        AgentEvent::ContextWarning {
            message, usage_pct, ..
        },
    )) = result
    {
        assert_eq!(level, crate::compact::TokenWarningState::Warning);
        assert!(message.contains("Approaching"));
        assert!(
            usage_pct <= 1.0,
            "pct should be ≤ 100%, got {:.0}%",
            usage_pct * 100.0
        );
    }
}

#[test]
fn test_build_context_warning_critical() {
    let threshold = crate::compact::get_auto_compact_threshold(TEST_CONTEXT_WINDOW);
    let at_80 = (threshold as f64 * 0.80) as u64;
    let result = build_context_warning(at_80, TEST_CONTEXT_WINDOW);
    assert!(result.is_some());
    if let Some((
        level,
        AgentEvent::ContextWarning {
            message, usage_pct, ..
        },
    )) = result
    {
        assert_eq!(level, crate::compact::TokenWarningState::Critical);
        assert!(message.contains("nearly full"));
        assert!(
            usage_pct <= 1.0,
            "pct should be ≤ 100%, got {:.0}%",
            usage_pct * 100.0
        );
    }
}

// ── make_continuation_message ────────────────────────────────────────

#[test]
fn test_continuation_first_attempt() {
    let msg = make_continuation_message(0, 3);
    let text = match &msg.content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text block"),
    };
    assert!(text.contains("Resume directly"));
}

#[test]
fn test_continuation_subsequent_attempt() {
    let msg = make_continuation_message(2, 5);
    let text = match &msg.content[0] {
        ContentBlock::Text { text } => text.clone(),
        _ => panic!("expected text block"),
    };
    assert!(text.contains("attempt 2/5"));
    assert!(text.contains("smaller pieces"));
}

// ── build_system_blocks ──────────────────────────────────────────────

#[test]
fn test_build_system_blocks_empty() {
    assert!(build_system_blocks("", false).is_none());
}

#[test]
fn test_build_system_blocks_no_boundary() {
    let blocks = build_system_blocks("Hello world", false).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].text, "Hello world");
    assert!(blocks[0].cache_control.is_some());
}

#[test]
fn test_build_system_blocks_with_boundary() {
    let boundary = crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
    let prompt = format!("Static part\n{}\nDynamic part", boundary);
    let blocks = build_system_blocks(&prompt, false).unwrap();
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].text.contains("Static part"));
    assert!(blocks[1].text.contains("Dynamic part"));
    assert!(blocks[0].cache_control.is_some());
    assert_eq!(
        blocks[0].cache_control.as_ref().unwrap().control_type,
        "ephemeral"
    );
    assert!(blocks[1].cache_control.is_none());
}

#[test]
fn test_build_system_blocks_boundary_strips_marker() {
    let boundary = crate::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
    let prompt = format!("Static\n{}\nDynamic data", boundary);
    let blocks = build_system_blocks(&prompt, false).unwrap();
    assert!(!blocks[1].text.contains(boundary));
    assert!(blocks[1].text.contains("Dynamic data"));
}

#[test]
fn test_build_system_blocks_skip_cache() {
    let blocks = build_system_blocks("Hello world", true).unwrap();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].cache_control.is_none());
}

// ── messages_to_api ──────────────────────────────────────────────────

#[test]
fn test_messages_to_api_converts_user_and_assistant() {
    let messages = vec![
        Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        }),
        Message::Assistant(AssistantMessage {
            uuid: "a1".into(),
            content: vec![ContentBlock::Text { text: "hi".into() }],
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        }),
    ];
    let api = messages_to_api(&messages, false, false);
    assert_eq!(api.len(), 2);
    assert_eq!(api[0].role, "user");
    assert_eq!(api[1].role, "assistant");
}

#[test]
fn test_messages_to_api_skips_system() {
    let messages = vec![
        Message::System(clawed_core::message::SystemMessage {
            uuid: "s1".into(),
            message: "system text".into(),
        }),
        Message::User(UserMessage {
            uuid: "u1".into(),
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        }),
    ];
    let api = messages_to_api(&messages, false, false);
    assert_eq!(api.len(), 1);
    assert_eq!(api[0].role, "user");
}

#[test]
fn test_messages_to_api_cache_control_on_last_block() {
    let messages = vec![Message::User(UserMessage {
        uuid: "u1".into(),
        content: vec![ContentBlock::Text {
            text: "hello".into(),
        }],
    })];
    let api = messages_to_api(&messages, false, false);
    match &api[0].content[0] {
        ApiContentBlock::Text { cache_control, .. } => {
            assert!(cache_control.is_some());
        }
        _ => panic!("expected Text block"),
    }
}

#[test]
fn test_messages_to_api_skip_cache() {
    let messages = vec![Message::User(UserMessage {
        uuid: "u1".into(),
        content: vec![ContentBlock::Text {
            text: "hello".into(),
        }],
    })];
    let api = messages_to_api(&messages, true, false);
    match &api[0].content[0] {
        ApiContentBlock::Text { cache_control, .. } => {
            assert!(
                cache_control.is_none(),
                "cache_control should be None when skip_cache=true"
            );
        }
        _ => panic!("expected Text block"),
    }
}

// ── block_to_api ─────────────────────────────────────────────────────

#[test]
fn test_block_to_api_text() {
    let block = ContentBlock::Text {
        text: "hello".into(),
    };
    let api = block_to_api(&block, false);
    match api {
        ApiContentBlock::Text {
            text,
            cache_control,
        } => {
            assert_eq!(text, "hello");
            assert!(cache_control.is_none());
        }
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_block_to_api_tool_use() {
    let block = ContentBlock::ToolUse {
        id: "t1".into(),
        name: "Bash".into(),
        input: serde_json::json!({"command": "ls"}),
    };
    let api = block_to_api(&block, false);
    match api {
        ApiContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "t1");
            assert_eq!(name, "Bash");
            assert_eq!(input["command"], "ls");
        }
        _ => panic!("expected ToolUse"),
    }
}

#[test]
fn test_block_to_api_thinking() {
    let block = ContentBlock::Thinking {
        signature: None,
        thinking: "let me think...".into(),
    };
    let api = block_to_api(&block, false);
    match api {
        ApiContentBlock::Text { text, .. } => {
            assert!(text.contains("<thinking>"));
            assert!(text.contains("let me think..."));
        }
        _ => panic!("expected Text for thinking"),
    }
}

#[test]
fn test_block_to_api_image() {
    let block = ContentBlock::Image {
        source: clawed_core::message::ImageSource {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        },
    };
    let api = block_to_api(&block, false);
    match api {
        ApiContentBlock::Image { source, .. } => {
            assert_eq!(source.source_type, "base64");
            assert_eq!(source.media_type, "image/png");
            assert_eq!(source.data, "iVBORw0KGgo=");
        }
        _ => panic!("expected Image for image block"),
    }
}

// ── QueryConfig ──────────────────────────────────────────────────────

#[test]
fn test_query_config_defaults() {
    let cfg = QueryConfig::default();
    assert_eq!(cfg.max_turns, 100);
    assert_eq!(cfg.max_tokens, 16384);
    assert!(cfg.system_prompt.is_empty());
    assert_eq!(cfg.token_budget, 0);
}

// ── Thinking preservation tests ─────────────────────────────────────────

#[test]
fn thinking_block_preserved_when_has_thinking_true() {
    // When has_thinking=true, ContentBlock::Thinking must become
    // ApiContentBlock::Thinking with both thinking text AND signature.
    let block = ContentBlock::Thinking {
        thinking: "let me think...".into(),
        signature: Some("sig_abc123".into()),
    };
    let api = block_to_api(&block, true);
    match api {
        ApiContentBlock::Thinking {
            thinking,
            signature,
        } => {
            assert_eq!(thinking, "let me think...");
            assert_eq!(
                signature,
                Some("sig_abc123".into()),
                "signature must be preserved for API chain continuation"
            );
        }
        ApiContentBlock::Text { text, .. } => {
            panic!(
                "thinking block was converted to text: '{}' — API will reject",
                text
            );
        }
        other => panic!("unexpected block type: {:?}", other),
    }
}

#[test]
fn thinking_block_without_signature_still_kept() {
    // Even without a signature, the thinking block type must be preserved.
    // Converting it to empty text causes the API to reject the request.
    let block = ContentBlock::Thinking {
        thinking: "hmm".into(),
        signature: None,
    };
    let api = block_to_api(&block, true);
    assert!(
        matches!(api, ApiContentBlock::Thinking { .. }),
        "thinking block without signature must remain as Thinking type"
    );
}

#[test]
fn compact_preserves_thinking_blocks() {
    // Compact must NOT convert thinking blocks to <thinking> XML text.
    // They must remain as ApiContentBlock::Thinking with signature intact.
    let messages = vec![Message::Assistant(AssistantMessage {
        uuid: "a1".into(),
        content: vec![
            ContentBlock::Thinking {
                thinking: "reason...".into(),
                signature: Some("sig_xyz".into()),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
        ],
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    })];
    let api = messages_to_api(&messages, false, true);
    assert_eq!(api.len(), 1);
    // First content block must be Thinking
    assert!(
        matches!(&api[0].content[0], ApiContentBlock::Thinking { signature, .. } if *signature == Some("sig_xyz".into())),
        "compact must preserve thinking block with signature"
    );
}

#[test]
fn pre_thinking_assistant_gets_text_wrapped() {
    // When has_thinking=true but an assistant message has no Thinking block
    // (e.g. from before /think was toggled on), its text must be wrapped
    // in a synthetic Thinking block to satisfy the API.
    let messages = vec![Message::Assistant(AssistantMessage {
        uuid: "a_old".into(),
        content: vec![ContentBlock::Text {
            text: "old response before thinking was enabled".into(),
        }],
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    })];
    let api = messages_to_api(&messages, false, true);
    assert_eq!(api.len(), 1);
    let has_thinking = api[0]
        .content
        .iter()
        .any(|b| matches!(b, ApiContentBlock::Thinking { .. }));
    assert!(
        has_thinking,
        "pre-thinking assistant message must have a thinking block injected"
    );
}

#[test]
fn content_block_stop_signature_parsed() {
    // ContentBlockStop must parse the optional signature field.
    // Without this, we lose the final thinking signature from the API.
    let json = serde_json::json!({
        "type": "content_block_stop",
        "index": 0,
        "signature": "final_sig_123"
    });
    let event: StreamEvent = serde_json::from_value(json).unwrap();
    match event {
        StreamEvent::ContentBlockStop { signature, index } => {
            assert_eq!(index, 0);
            assert_eq!(
                signature,
                Some("final_sig_123".into()),
                "ContentBlockStop must capture the thinking signature"
            );
        }
        other => panic!("expected ContentBlockStop, got: {:?}", other),
    }
}

#[test]
fn content_block_stop_without_signature_still_works() {
    // ContentBlockStop for non-thinking blocks won't have a signature.
    let json = serde_json::json!({
        "type": "content_block_stop",
        "index": 1
    });
    let event: StreamEvent = serde_json::from_value(json).unwrap();
    match event {
        StreamEvent::ContentBlockStop { signature, index } => {
            assert_eq!(index, 1);
            assert_eq!(
                signature, None,
                "non-thinking ContentBlockStop should have None signature"
            );
        }
        other => panic!("expected ContentBlockStop"),
    }
}

#[test]
fn thinking_config_includes_signature_for_chain() {
    // When a signature is present (from previous turn), it must be
    // included in the thinking config for the next request.
    use clawed_api::types::ThinkingConfig;
    let cfg = ThinkingConfig {
        thinking_type: "enabled".into(),
        budget_tokens: Some(10000),
        signature: Some("prev_sig".into()),
    };
    let json = serde_json::to_value(&cfg).unwrap();
    assert_eq!(json["type"], "enabled");
    assert_eq!(
        json["signature"], "prev_sig",
        "signature must be serialized in the thinking config"
    );
}

#[test]
fn thinking_config_without_signature_omits_field() {
    // First turn: no signature yet, field should be absent from JSON.
    use clawed_api::types::ThinkingConfig;
    let cfg = ThinkingConfig {
        thinking_type: "enabled".into(),
        budget_tokens: Some(10000),
        signature: None,
    };
    let json = serde_json::to_value(&cfg).unwrap();
    assert!(
        json.get("signature").is_none(),
        "first-turn thinking config must omit signature field"
    );
}
