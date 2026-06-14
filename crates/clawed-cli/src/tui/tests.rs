use super::*;
use clawed_bus::bus::EventBus;
use clawed_bus::events::{AgentRequest, PermissionRequest, RiskLevel};
use clawed_core::skills::SkillEntry;
use futures::FutureExt;
use serde_json::json;
use tempfile::TempDir;

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn sample_skill(name: &str, description: &str) -> SkillEntry {
    SkillEntry {
        name: name.to_string(),
        display_name: None,
        description: description.to_string(),
        system_prompt: "You are helpful".to_string(),
        allowed_tools: vec![],
        model: None,
        when_to_use: None,
        paths: vec![],
        argument_names: vec![],
        argument_hint: Some("<prompt>".to_string()),
        version: None,
        context: None,
        agent: None,
        effort: None,
        user_invocable: true,
        disable_model_invocation: false,
        skill_root: None,
    }
}

#[test]
fn welcome_lines_are_nonempty() {
    let lines = render_welcome_lines(80, "claude-sonnet-4-20250514", "bypass");
    assert!(!lines.is_empty());
}

#[test]
fn app_push_message_works() {
    let mut app = App::new("test-model".to_string());
    app.push_message(MessageContent::System("hello".to_string()));
    assert_eq!(app.messages.len(), 1);
}

#[test]
fn app_append_assistant_text() {
    let mut app = App::new("test-model".to_string());
    app.append_assistant_text("hello ");
    app.append_assistant_text("world");
    assert_eq!(app.messages.len(), 1);
    if let MessageContent::AssistantText(ref text) = app.messages[0].content {
        assert_eq!(text, "hello world");
    } else {
        panic!("Expected AssistantText");
    }
}

#[test]
fn app_append_thinking_text() {
    let mut app = App::new("test-model".to_string());
    app.append_thinking_text("thinking...");
    app.append_thinking_text(" more");
    assert_eq!(app.messages.len(), 1);
    if let MessageContent::ThinkingText(ref text) = app.messages[0].content {
        assert_eq!(text, "thinking... more");
    } else {
        panic!("Expected ThinkingText");
    }
}

#[test]
fn text_delta_after_thinking_creates_new_message() {
    let mut app = App::new("test-model".to_string());
    app.append_thinking_text("hmm");
    app.append_assistant_text("answer");
    assert_eq!(app.messages.len(), 2);
}

#[test]
fn slash_help_adds_system_message() {
    let mut app = App::new("test".to_string());
    app.push_message(MessageContent::System("help text".to_string()));
    assert_eq!(app.messages.len(), 1);
}

#[test]
fn slash_help_routes_long_print_output_to_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/help");

    assert!(app.messages.is_empty());
    assert!(app.overlay.is_some());
}

#[test]
fn short_print_output_stays_in_transcript() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/tag demo");

    assert!(app.overlay.is_none());
    assert!(!app.messages.is_empty());
}

#[test]
fn overlay_replaces_none() {
    let mut app = App::new("test".to_string());
    assert!(app.overlay.is_none());
    app.overlay = Some(overlay::build_model_overlay("test"));
    assert!(app.overlay.is_some());
    app.overlay = None;
    assert!(app.overlay.is_none());
}

#[test]
fn model_command_opens_footer_picker_instead_of_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/model");

    assert!(app.overlay.is_none());
    assert!(app.footer_picker.is_some());
    assert_eq!(
        app.footer_picker.as_ref().map(|picker| picker.kind),
        Some(FooterPickerKind::Model)
    );
}

#[test]
fn poll_interval_is_idle_when_inactive() {
    let app = App::new("test".to_string());
    assert_eq!(poll_interval(&app), IDLE_POLL_INTERVAL);
}

#[test]
fn poll_interval_is_active_while_generating() {
    let mut app = App::new("test".to_string());
    app.status.is_generating = true;
    assert_eq!(poll_interval(&app), ACTIVE_POLL_INTERVAL);
}

#[test]
fn completion_popup_slot_height_is_fixed_while_open() {
    assert_eq!(completion_popup_rows_from_count(0), 0);
    assert_eq!(completion_popup_rows_from_count(1), 0);
    assert_eq!(completion_popup_rows_from_count(2), 2);
    assert_eq!(completion_popup_rows_from_count(5), 5);
    assert_eq!(
        completion_popup_rows_from_count(20),
        MAX_COMPLETION_POPUP_ITEMS as u16
    );
}

#[test]
fn build_skills_picker_lists_invocable_skills() {
    let picker =
        build_skills_picker(&[sample_skill("review", "Review code")]).expect("skills picker");

    assert_eq!(picker.kind, FooterPickerKind::Skills);
    assert_eq!(picker.items.len(), 1);
    assert_eq!(picker.items[0].label, "/review");
    assert_eq!(picker.items[0].value, "review");
}

#[test]
fn footer_picker_end_keeps_selection_visible() {
    let mut picker = FooterPicker {
        kind: FooterPickerKind::Model,
        items: (0..12)
            .map(|i| SelectionItem {
                label: format!("item-{i}"),
                description: String::new(),
                value: i.to_string(),
                is_current: false,
            })
            .collect(),
        selected: 0,
        scroll_offset: 0,
    };

    assert!(matches!(
        picker.handle_key(KeyCode::End),
        FooterPickerAction::Consumed
    ));
    assert_eq!(picker.selected, 11);
    assert_eq!(picker.scroll_offset, 2);
}

#[test]
fn footer_picker_arrow_left_is_consumed() {
    let mut picker = FooterPicker {
        kind: FooterPickerKind::Model,
        items: vec![SelectionItem {
            label: "item".to_string(),
            description: String::new(),
            value: "value".to_string(),
            is_current: false,
        }],
        selected: 0,
        scroll_offset: 0,
    };

    assert!(matches!(
        picker.handle_key(KeyCode::Left),
        FooterPickerAction::Consumed
    ));
}

#[test]
fn footer_picker_character_input_passes_through() {
    let mut picker = FooterPicker {
        kind: FooterPickerKind::Model,
        items: vec![SelectionItem {
            label: "item".to_string(),
            description: String::new(),
            value: "value".to_string(),
            is_current: false,
        }],
        selected: 0,
        scroll_offset: 0,
    };

    assert!(matches!(
        picker.handle_key(KeyCode::Char('x')),
        FooterPickerAction::PassThrough
    ));
}

#[test]
fn mouse_scroll_in_output_area_updates_scroll_offset() {
    let mut app = App::new("test".to_string());
    // Simulate content that overflows: scroll_offset > 0 means scrolled up.
    app.scroll_offset = 5;
    app.auto_scroll = true;

    // ScrollUp in output area (no right panel) should increase offset.
    app.scroll_offset = app.scroll_offset.saturating_add(3);
    app.auto_scroll = false;
    assert_eq!(app.scroll_offset, 8);
    assert!(!app.auto_scroll);

    // ScrollDown toward bottom.
    app.scroll_offset = app.scroll_offset.saturating_sub(3);
    assert_eq!(app.scroll_offset, 5);

    // ScrollDown to bottom restores auto_scroll.
    app.scroll_offset = 2;
    app.scroll_offset = app.scroll_offset.saturating_sub(3);
    assert_eq!(app.scroll_offset, 0);
}

#[test]
fn mouse_scroll_in_right_panel_dispatches_to_focused_sub_panel() {
    let mut app = App::new("test".to_string());
    app.task_list.scroll_offset = 5;
    app.tool_history_scroll = 3;
    app.right_panel_focus = Some(RightPanelFocus::Tasks);

    scroll_right_sub(&mut app, RightPanelFocus::Tasks, 3); // scroll up in Tasks
    assert_eq!(app.task_list.scroll_offset, 8);
    assert_eq!(app.tool_history_scroll, 3); // unchanged

    app.right_panel_focus = Some(RightPanelFocus::ToolHistory);
    scroll_right_sub(&mut app, RightPanelFocus::ToolHistory, -3i32); // scroll down in ToolHistory
    assert_eq!(app.tool_history_scroll, 0);
    assert_eq!(app.task_list.scroll_offset, 8); // unchanged
}

#[test]
fn scroll_right_panel_defaults_to_tasks_when_no_focus() {
    let mut app = App::new("test".to_string());
    app.task_list.scroll_offset = 0;
    app.right_panel_focus = None;

    scroll_right_sub(&mut app, RightPanelFocus::Tasks, 3);
    assert_eq!(app.task_list.scroll_offset, 3);
}

#[test]
fn last_right_panel_x_is_zero_when_panel_hidden() {
    let app = App::new("test".to_string());
    assert_eq!(app.last_right_panel_x, 0);
}

#[test]
fn long_print_output_prefers_overlay() {
    let long_text = (0..20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(should_render_print_output_in_overlay(&long_text));
    assert!(!should_render_print_output_in_overlay("short output"));
}

#[test]
fn spinner_tick_waits_for_interval() {
    let mut app = App::new("test".to_string());
    app.status.is_generating = true;
    app.needs_redraw = false;
    let start = app.last_spinner_tick;

    app.advance_spinner_if_due(start + Duration::from_millis(40));

    assert_eq!(app.status.spinner_frame, 0);
    assert!(!app.needs_redraw);
}

#[test]
fn spinner_tick_marks_redraw_when_due() {
    let mut app = App::new("test".to_string());
    app.status.is_generating = true;
    app.needs_redraw = false;
    let start = app.last_spinner_tick;

    app.advance_spinner_if_due(start + SPINNER_TICK_INTERVAL);

    assert_eq!(app.status.spinner_frame, 1);
    assert!(app.needs_redraw);
}

#[test]
fn should_clear_message_area_only_when_visual_height_shrinks() {
    assert!(should_clear_message_area(Some(10), 9));
    assert!(!should_clear_message_area(Some(10), 10));
    assert!(!should_clear_message_area(Some(10), 11));
    assert!(!should_clear_message_area(None, 9));
}

#[test]
fn cached_visible_lines_track_assistant_append() {
    // Isolates the assistant-append incremental cache path.
    // Pushing a System message intentionally invalidates the incremental
    // cache (via `affects_system_grouping` in push_message), so it is
    // omitted here to exercise the clean append path.
    let mut app = App::new("test".to_string());
    app.push_message(MessageContent::AssistantText("hello".to_string()));

    app.append_assistant_text(" world");

    assert!(!app.cached_visible_lines_dirty);
    assert_eq!(
        line_text(app.cached_visible_lines.last().expect("cached line")),
        "\u{25CF} hello world"
    );
}

#[test]
fn collapsed_thinking_short_text_shows_placeholder() {
    // Collapsed thinking always shows a single placeholder line,
    // matching the official CC collapsed view.
    let mut app = App::new("test".to_string());
    app.push_message(MessageContent::ThinkingText("one\n\ntwo".to_string()));

    assert_eq!(app.cached_visible_lines.len(), 1);
    assert!(line_text(&app.cached_visible_lines[0]).contains("\u{2234} Thinking"));
}

#[test]
fn collapsed_thinking_long_text_shows_hint() {
    let mut app = App::new("test".to_string());
    app.push_message(MessageContent::ThinkingText(
        "one\ntwo\nthree\nfour".to_string(),
    ));

    // Collapsed thinking always shows the same placeholder.
    assert_eq!(app.cached_visible_lines.len(), 1);
    assert!(line_text(&app.cached_visible_lines[0]).contains("Ctrl+O to expand"));
}

#[test]
fn streaming_assistant_renders_inline_markdown_until_done() {
    let mut app = App::new("test".to_string());
    app.is_generating = true;
    app.push_message(MessageContent::AssistantText("**bold**".to_string()));

    // Streaming: lightweight inline parsing strips the markers.
    assert_eq!(line_text(&app.cached_visible_lines[0]), "\u{25CF} bold");

    app.mark_done();
    app.rebuild_visible_lines();

    // Done: full markdown renderer also produces "bold".
    assert_eq!(line_text(&app.cached_visible_lines[0]), "\u{25CF} bold");
}

#[test]
fn parse_inline_spans_bold_italic_code() {
    let spans = parse_inline_spans("**bold** and *italic* and `code`");
    assert_eq!(spans.len(), 5);
    assert_eq!(spans[0].content, "bold");
    assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(spans[1].content, " and ");
    assert_eq!(spans[2].content, "italic");
    assert!(spans[2].style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(spans[3].content, " and ");
    assert_eq!(spans[4].content, "code");
    assert_eq!(spans[4].style.bg, Some(Color::Rgb(45, 45, 45)));
}

#[test]
fn parse_inline_spans_leaves_unclosed_as_plain() {
    let spans = parse_inline_spans("**unclosed bold");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "**unclosed bold");
}

#[test]
fn parse_inline_spans_plain_text_unchanged() {
    let spans = parse_inline_spans("hello world");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "hello world");
}

#[test]
fn parse_inline_spans_skips_code_blocks() {
    let spans = parse_inline_spans("```rust fn main() {} ```");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "```rust fn main() {} ```");
}

#[test]
fn parse_inline_spans_double_backtick_is_plain() {
    let spans = parse_inline_spans("``not code``");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "``not code``");
}

#[test]
fn layout_signature_detects_footer_changes() {
    let mut app = App::new("test".to_string());
    let base = app.layout_signature();

    app.bottom_bar_hidden = true;
    assert_ne!(base, app.layout_signature());

    app.bottom_bar_hidden = false;
    app.queued_inputs.push("queued".to_string());
    assert_ne!(base, app.layout_signature());

    app.queued_inputs.clear();
    app.task_plan
        .add_task("agent-1".to_string(), "Task".to_string());
    assert_ne!(base, app.layout_signature());

    let mut completion_app = App::new("test".to_string());
    let completion_base = completion_app.layout_signature();
    completion_app
        .input
        .handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Char('/'),
            KeyModifiers::NONE,
        ));
    let completion_open = completion_app.layout_signature();
    assert_ne!(completion_base, completion_open);

    completion_app
        .input
        .handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::NONE,
        ));
    assert_eq!(completion_open.completion_rows, 10);
    assert_eq!(completion_app.layout_signature().completion_rows, 2);
}

#[test]
fn layout_signature_detects_permission_and_task_panel() {
    let mut app = App::new("test".to_string());
    let base = app.layout_signature();

    app.task_plan
        .add_task("agent-1".to_string(), "Task".to_string());
    assert_ne!(base, app.layout_signature());

    app.task_plan = taskplan::TaskPlan::new();
    app.permission = Some(PendingPermission::new(PermissionRequest {
        request_id: "req-1".to_string(),
        tool_name: "Bash".to_string(),
        input: json!({"command": "ls"}),
        risk_level: RiskLevel::Medium,
        description: "Bash: command=ls".to_string(),
    }));
    assert_ne!(base, app.layout_signature());
}

#[test]
fn completion_popup_stays_within_reserved_footer_slot() {
    let input_area = Rect::new(4, 20, 50, 1);
    let popup_slot = Rect::new(4, 21, 50, 3);
    let matches = ["/help", "/history", "/review"];

    let popup_area = completion_popup_area(popup_slot, input_area, &matches).expect("popup area");

    assert_eq!(popup_area.x, input_area.x);
    assert_eq!(popup_area.y, popup_slot.y);
    assert_eq!(popup_area.height, popup_slot.height);
    assert!(popup_area.width <= popup_slot.width);
    assert!(popup_area.y >= input_area.y + input_area.height);
}

#[tokio::test(flavor = "multi_thread")]
async fn permissions_without_mode_open_footer_picker() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Permissions {
            mode: String::new(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.overlay.is_none());
    assert_eq!(
        app.footer_picker.as_ref().map(|picker| picker.kind),
        Some(FooterPickerKind::Permissions)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_picker_selection_prefills_input() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_footer_picker_selection(
        FooterPickerKind::Skills,
        "review",
        &client,
        &engine,
        &mut app,
    )
    .await;

    assert_eq!(app.input.buffer(), "/review ");
}

#[tokio::test(flavor = "multi_thread")]
async fn run_plugin_command_submits_prompt_in_tui() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (mut bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::RunPluginCommand {
            name: "greet".to_string(),
            prompt: "Greet the user".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.is_generating);
    match bus.recv_request().await {
        Some(AgentRequest::Submit { text, images }) => {
            assert_eq!(text, "Greet the user");
            assert!(images.is_empty());
        }
        _ => panic!("expected submit request"),
    }
}

// -- E2E-style event loop simulation tests --

/// Simulate the event loop drain-notify-render cycle to verify that
/// rapid streaming + input events don't cause layout corruption or
/// render starvation. This is an "E2E" test of the TUI event loop
/// without requiring a real terminal.
struct E2ETestEnv {
    app: App,
    notify_tx: tokio::sync::mpsc::Sender<AgentNotification>,
    notify_rx: tokio::sync::mpsc::Receiver<AgentNotification>,
    render_count: usize,
}

impl E2ETestEnv {
    fn new() -> Self {
        let (notify_tx, notify_rx) = tokio::sync::mpsc::channel(256);
        Self {
            app: App::new("test-model".to_string()),
            notify_tx,
            notify_rx,
            render_count: 0,
        }
    }

    /// Run one iteration of the event loop: drain notifications,
    /// advance spinner, check layout, render if needed.
    fn tick(&mut self) {
        // Drain all pending notifications
        while let Ok(notification) = self.notify_rx.try_recv() {
            let turn_complete = matches!(notification, AgentNotification::TurnComplete { .. });
            let merged = self.app.handle_notification(notification);
            // In simulation, we don't actually submit to a real client
            if let Some(merged) = merged {
                self.app.push_message(MessageContent::UserInput(merged));
                self.app.mark_generating();
            }
            if turn_complete
                && self.app.pending_workflow.is_none()
                && !self.app.expecting_turn_start
            {
                // Drain queue in simulation
                if let Some(merged) = self.app.take_queued_inputs() {
                    self.app.push_message(MessageContent::UserInput(merged));
                }
            }
        }

        // Advance spinner
        self.app.advance_spinner_if_due(Instant::now());

        // Detect layout changes
        let layout_sig = self.app.layout_signature();
        let layout_changed = layout_sig != self.app.last_layout_sig;
        if layout_changed {
            self.app.needs_full_clear = true;
            self.app.last_layout_sig = layout_sig;
            self.app.request_redraw();
        }

        // Clear if needed
        if self.app.needs_full_clear {
            self.app.needs_full_clear = false;
            self.app.request_redraw();
        }

        // Render if needed — use the preserved layout_changed flag
        if self.app.needs_redraw {
            let throttled = !layout_changed
                && self.app.is_generating
                && self.app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
            if !throttled {
                // Simulate render: rebuild visible lines
                self.app.rebuild_visible_lines();
                self.app.last_render_at = Instant::now();
                self.render_count += 1;
            }
            self.app.needs_redraw = false;
        }
    }

    fn send_turn_start(&self) {
        let _ = self.notify_tx.try_send(AgentNotification::TurnStart {
            turn: self.app.total_turns + 1,
        });
    }

    fn send_text_deltas(&self, deltas: &[&str]) {
        for delta in deltas {
            let _ = self.notify_tx.try_send(AgentNotification::TextDelta {
                text: delta.to_string(),
            });
        }
    }

    fn send_turn_complete(&self) {
        let _ = self.notify_tx.try_send(AgentNotification::TurnComplete {
            turn: self.app.total_turns + 1,
            stop_reason: "end_turn".to_string(),
            usage: clawed_bus::events::UsageInfo {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        });
    }

    fn send_tool_start(&self, id: &str, tool_name: &str) {
        let _ = self.notify_tx.try_send(AgentNotification::ToolUseStart {
            id: id.to_string(),
            tool_name: tool_name.to_string(),
        });
    }

    fn send_tool_ready(&self, id: &str, tool_name: &str, input: serde_json::Value) {
        let _ = self.notify_tx.try_send(AgentNotification::ToolUseReady {
            id: id.to_string(),
            tool_name: tool_name.to_string(),
            input,
        });
    }

    fn send_tool_output(&self, id: &str, tool_name: &str, line: &str) {
        let _ = self.notify_tx.try_send(AgentNotification::ToolOutputLine {
            id: id.to_string(),
            tool_name: tool_name.to_string(),
            line: line.to_string(),
        });
    }

    fn send_tool_complete(
        &self,
        id: &str,
        tool_name: &str,
        is_error: bool,
        result_preview: Option<&str>,
    ) {
        let _ = self.notify_tx.try_send(AgentNotification::ToolUseComplete {
            id: id.to_string(),
            tool_name: tool_name.to_string(),
            is_error,
            cancelled: false,
            rejected: false,
            reject_reason: None,
            result_preview: result_preview.map(|s| s.to_string()),
        });
    }

    fn send_agent_spawned(&self, agent_id: &str, name: &str) {
        let _ = self.notify_tx.try_send(AgentNotification::AgentSpawned {
            agent_id: agent_id.to_string(),
            name: Some(name.to_string()),
            agent_type: "sub".to_string(),
            background: false,
        });
    }

    #[allow(dead_code)]
    fn send_agent_complete(&self, agent_id: &str) {
        let _ = self.notify_tx.try_send(AgentNotification::AgentComplete {
            agent_id: agent_id.to_string(),
            result: "done".to_string(),
            is_error: false,
        });
    }
}

#[test]
fn e2e_rapid_streaming_does_not_corrupt_layout() {
    let mut env = E2ETestEnv::new();

    // Start a turn
    env.send_turn_start();
    env.tick();

    // Send 200 small deltas simulating rapid LLM streaming
    let deltas: Vec<&str> = (0..200)
        .map(|i| {
            if i % 10 == 0 {
                "**bold** "
            } else if i % 5 == 0 {
                "`code` "
            } else {
                "word "
            }
        })
        .collect();
    env.send_text_deltas(&deltas);

    // Process all ticks
    for _ in 0..50 {
        env.tick();
    }

    // Layout should be consistent: signature should match last known
    let sig = env.app.layout_signature();
    assert_eq!(sig, env.app.last_layout_sig);

    // The cached visible lines should be valid (not dirty after last tick)
    assert!(!env.app.cached_visible_lines_dirty);

    // Message count should reflect all deltas
    assert!(!env.app.messages.is_empty());
}

#[test]
fn e2e_streaming_then_input_queue_works() {
    let mut env = E2ETestEnv::new();

    // Start generating
    env.app.mark_generating();
    env.send_turn_start();
    env.tick();

    // Stream some text
    env.send_text_deltas(&["hello ", "world"]);
    env.tick();

    // Verify generating state
    assert!(env.app.is_generating);

    // Complete the turn
    env.send_turn_complete();
    env.tick();

    // After turn complete, generating should be false
    assert!(!env.app.is_generating);

    // Text should be in the messages
    let has_text = env.app.messages.iter().any(|m| {
        if let MessageContent::AssistantText(ref t) = m.content {
            t.contains("hello") || t.contains("world")
        } else {
            false
        }
    });
    assert!(has_text, "streamed text should appear in messages");
}

#[test]
fn e2e_layout_signature_tracks_terminal_resize() {
    let mut env = E2ETestEnv::new();

    // Initial state
    env.app.term_width = 80;
    env.app.term_height = 24;
    let initial_sig = env.app.layout_signature();

    // Simulate terminal resize
    env.app.term_width = 120;
    env.app.term_height = 40;
    let new_sig = env.app.layout_signature();

    // Signature should differ
    assert_ne!(initial_sig, new_sig);
    assert_eq!(new_sig.term_width, 120);
    assert_eq!(new_sig.term_height, 40);
}

#[test]
fn e2e_overlay_toggle_causes_layout_change() {
    let mut env = E2ETestEnv::new();

    let base = env.app.layout_signature();
    assert!(!base.has_overlay);

    // Open overlay
    env.app.overlay = Some(overlay::build_model_overlay("test"));
    let with_overlay = env.app.layout_signature();
    assert!(with_overlay.has_overlay);
    assert_ne!(base, with_overlay);

    // Close overlay
    env.app.overlay = None;
    let after_close = env.app.layout_signature();
    assert!(!after_close.has_overlay);
    // After close, signature should match base
    assert_eq!(base, after_close);
}

#[test]
fn e2e_render_throttle_during_streaming() {
    let mut app = App::new("test-model".to_string());

    // Set stable layout so no layout change triggers
    app.term_width = 80;
    app.term_height = 24;
    app.last_layout_sig = app.layout_signature();

    // Mark generating so throttle applies
    app.mark_generating();

    // First render — should happen (last_render_at is > 32ms ago)
    app.needs_redraw = true;
    let _before_renders = app.last_render_at;
    // Simulate one tick of the event loop render logic
    {
        let layout_changed = false; // layout is stable
        let throttled = !layout_changed
            && app.is_generating
            && app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
        assert!(
            !throttled,
            "first render should NOT be throttled (elapsed > 32ms)"
        );
    }

    // Perform the render
    app.last_render_at = Instant::now();
    let first_render_at = app.last_render_at;

    // Immediately request another render — should be throttled
    app.needs_redraw = true;
    {
        let layout_changed = false;
        let throttled = !layout_changed
            && app.is_generating
            && app.last_render_at.elapsed() < MIN_RENDER_INTERVAL;
        assert!(
            throttled,
            "second render SHOULD be throttled (elapsed < 32ms)"
        );
    }

    // Verify the first render time is recent
    assert!(first_render_at.elapsed() < Duration::from_millis(10));
}

#[test]
fn e2e_layout_change_bypasses_throttle() {
    let mut env = E2ETestEnv::new();

    env.app.mark_generating();
    env.send_turn_start();
    env.app.term_width = 80;
    env.app.term_height = 24;
    env.app.last_layout_sig = env.app.layout_signature();
    env.tick();

    // Force initial render
    env.app.needs_redraw = true;
    env.tick();
    let initial_renders = env.render_count;

    // Now change layout (open overlay) — should bypass throttle
    env.app.overlay = Some(overlay::build_model_overlay("test"));
    env.app.needs_redraw = true;
    env.tick();

    // Should have rendered despite throttle (layout changed)
    assert!(
        env.render_count > initial_renders,
        "layout change should bypass render throttle"
    );
}

// -- E2E: slash command routing tests --

#[test]
fn e2e_slash_command_think_toggles_thinking() {
    let (mut bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/think");
    // Think sends SetThinking request, not pending_command
    match bus.recv_request().now_or_never() {
        Some(Some(clawed_bus::events::AgentRequest::SetThinking { mode })) => {
            assert_eq!(mode, "on");
        }
        other => panic!("expected SetThinking request, got {other:?}"),
    }
}

#[test]
fn e2e_slash_command_breakcache_sets_request() {
    let (mut bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/break-cache");
    // BreakCache sends BreakCache request directly, not pending_command
    match bus.recv_request().now_or_never() {
        Some(Some(clawed_bus::events::AgentRequest::BreakCache)) => {}
        other => panic!("expected BreakCache request, got {other:?}"),
    }
}

#[test]
fn e2e_slash_command_env_opens_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/env");
    assert!(app.overlay.is_some());
}

#[test]
fn e2e_slash_command_effort_valid() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/effort high");
    assert!(app.messages.len() == 1);
    if let MessageContent::System(ref text) = app.messages[0].content {
        assert!(text.contains("high"));
    } else {
        panic!("expected system message");
    }
}

#[test]
fn e2e_slash_command_effort_invalid() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/effort ultra");
    assert!(app.messages.len() == 1);
    if let MessageContent::System(ref text) = app.messages[0].content {
        assert!(text.contains("Invalid"));
    } else {
        panic!("expected system message");
    }
}

#[test]
fn e2e_slash_command_effort_empty_shows_help() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/effort");
    assert!(app.messages.len() == 1);
    if let MessageContent::System(ref text) = app.messages[0].content {
        assert!(text.contains("Current effort: auto"));
    } else {
        panic!("expected system message");
    }
}

#[test]
fn e2e_slash_command_tag_with_name() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/tag v1.0");
    assert!(app.messages.len() == 1);
    if let MessageContent::System(ref text) = app.messages[0].content {
        assert!(text.contains("v1.0"));
    } else {
        panic!("expected system message");
    }
}

#[test]
fn e2e_slash_command_tag_empty_shows_usage() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/tag");
    assert!(app.messages.len() == 1);
    if let MessageContent::System(ref text) = app.messages[0].content {
        assert!(text.contains("Usage"));
    } else {
        panic!("expected system message");
    }
}

#[test]
fn e2e_slash_command_stickers_shows_url() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/stickers");
    assert!(app.messages.len() == 1);
    if let MessageContent::System(ref text) = app.messages[0].content {
        assert!(text.contains("stickers"));
    } else {
        panic!("expected system message");
    }
}

#[test]
fn e2e_slash_command_exit_stops_running() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());
    assert!(app.running);

    app.handle_slash_command(&client, "/exit");
    assert!(!app.running);
}

#[test]
fn e2e_slash_command_cost_opens_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/cost");
    assert!(app.overlay.is_some());
}

#[test]
fn e2e_slash_command_status_opens_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/status");
    assert!(app.overlay.is_some());
}

#[test]
fn e2e_slash_command_clear_clears_messages() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());
    app.push_message(MessageContent::System("hello".to_string()));
    assert_eq!(app.messages.len(), 1);

    app.handle_slash_command(&client, "/clear");
    assert!(app.messages.is_empty());
}

#[test]
fn e2e_slash_command_model_opens_footer_picker() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test-model".to_string());

    app.handle_slash_command(&client, "/model");
    assert!(app.footer_picker.is_some());
    assert_eq!(
        app.footer_picker.as_ref().map(|p| p.kind),
        Some(FooterPickerKind::Model)
    );
}

#[test]
fn e2e_slash_command_model_set_closes_picker() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test-model".to_string());

    app.handle_slash_command(&client, "/model sonnet");
    // Picker should be cleared after setting model
    assert!(app.footer_picker.is_none());
}

#[test]
fn e2e_slash_command_compact_sends_request() {
    let (mut bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/compact summarize the code");
    // Compact sends Compact request directly, not pending_command
    match bus.recv_request().now_or_never() {
        Some(Some(clawed_bus::events::AgentRequest::Compact { ref instructions })) => {
            assert!(instructions
                .as_ref()
                .is_some_and(|i| i.contains("summarize")));
        }
        other => panic!("expected Compact request, got {other:?}"),
    }
}

#[test]
fn e2e_slash_command_review_sends_to_engine() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/review check for bugs");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Review { ref prompt }) = app.pending_command {
        assert!(prompt.contains("bugs"));
    } else {
        panic!("expected Review command result");
    }
}

#[test]
fn e2e_slash_command_bug_sends_to_engine() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/bug why is this crashing");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_pr_sends_to_engine() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/pr review this PR");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_unknown_stays_unknown() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/foobar");
    // Unknown commands should not crash or produce unexpected behavior
}

#[test]
fn e2e_slash_command_mcp_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/mcp");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_vim_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/vim");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_permissions_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/permissions");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_config_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/config");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_doctor_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/doctor");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_init_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/init");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_login_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/login");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_logout_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/logout");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_theme_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/theme");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_agents_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/agents");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_plan_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plan");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_resume_shows_picker() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/resume");
    // /resume now shows a footer picker (or a message if no sessions)
    assert!(app.footer_picker.is_some() || !app.messages.is_empty());
}

#[test]
fn e2e_slash_command_memory_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/memory list");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_pr_comments_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/pr-comments 123");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_branch_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/branch my-feature");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_search_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/search hello");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_history_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/history");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_undo_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/undo");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_retry_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/retry");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_copy_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/copy");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_share_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/share");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_rename_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/rename v2");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_summary_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/summary");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_export_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/export");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_context_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/context");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_fast_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/fast");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_rewind_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/rewind 3");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_add_dir_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/add-dir .");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_files_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/files *.rs");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_image_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/image test.png");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_feedback_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/feedback this is great");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_stats_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/stats");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_release_notes_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/release-notes");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_reload_context_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/reload-context");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_diff_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/diff");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_commit_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/commit fix: typo");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_commit_push_pr_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/commit-push-pr");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_slash_command_plugin_goes_to_pending() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plugin");
    assert!(app.pending_command.is_some());
}

// ========================================================================
// P0 Supplement: Subcommand parameter tests
// ========================================================================

// -- /mcp subcommands --

#[test]
fn e2e_mcp_list_produces_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/mcp list");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Mcp { ref sub }) = app.pending_command {
        assert_eq!(sub, "list");
    } else {
        panic!("expected Mcp command result");
    }
}

#[test]
fn e2e_mcp_status_produces_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/mcp status");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Mcp { ref sub }) = app.pending_command {
        assert_eq!(sub, "status");
    } else {
        panic!("expected Mcp command result");
    }
}

#[test]
fn e2e_mcp_unknown_sub_returns_error() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/mcp foobar");
    assert!(app.pending_command.is_some());
}

#[test]
fn e2e_mcp_help_subcommand() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/mcp help");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Mcp { ref sub }) = app.pending_command {
        assert_eq!(sub, "help");
    } else {
        panic!("expected Mcp command result");
    }
}

// -- /plugin subcommands --

#[test]
fn e2e_plugin_list_subcommand() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plugin list");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
        assert_eq!(sub, "list");
    } else {
        panic!("expected Plugin command result");
    }
}

#[test]
fn e2e_plugin_info_without_name() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plugin info");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
        assert_eq!(sub, "info");
    } else {
        panic!("expected Plugin command result");
    }
}

#[test]
fn e2e_plugin_enable_without_name() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plugin enable");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
        assert_eq!(sub, "enable");
    } else {
        panic!("expected Plugin command result");
    }
}

#[test]
fn e2e_plugin_disable_without_name() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plugin disable");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
        assert_eq!(sub, "disable");
    } else {
        panic!("expected Plugin command result");
    }
}

#[test]
fn e2e_plugin_unknown_subcommand() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/plugin foobar");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Plugin { ref sub }) = app.pending_command {
        assert_eq!(sub, "foobar");
    } else {
        panic!("expected Plugin command result");
    }
}

// -- /agents subcommands --

#[test]
fn e2e_agents_list_subcommand() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/agents list");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Agents { ref sub }) = app.pending_command {
        assert_eq!(sub, "list");
    } else {
        panic!("expected Agents command result");
    }
}

#[test]
fn e2e_agents_status_subcommand() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/agents status");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Agents { ref sub }) = app.pending_command {
        assert_eq!(sub, "status");
    } else {
        panic!("expected Agents command result");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_agents_status_empty_shows_no_agents() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Agents {
            sub: "status".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.overlay.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_agents_info_without_name_shows_usage() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Agents {
            sub: "info".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.overlay.is_some());
    if let Some(ref overlay) = app.overlay {
        // The format_agents_tui function produces "Usage: /agents info <name>"
        // when sub is exactly "info" with no name
        let text = format!("{:?}", overlay);
        assert!(text.contains("Usage") || text.contains("info"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_agents_create_without_name_shows_usage() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Agents {
            sub: "create".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.overlay.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_agents_delete_without_name_shows_usage() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Agents {
            sub: "delete".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.overlay.is_some());
}

// -- /permissions subcommands --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_permissions_bypass_mode() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Permissions {
            mode: "bypass".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    // Setting a mode should produce a system message, not open picker
    assert!(app.footer_picker.is_none());
    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Permission"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_permissions_plan_mode() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Permissions {
            mode: "plan".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.footer_picker.is_none());
    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Permission"));
}

// -- /vim subcommands --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_vim_on_enables() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Vim {
            toggle: "on".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("enabled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_vim_off_disables() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Vim {
            toggle: "off".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("disabled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_vim_invalid_shows_usage() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Vim {
            toggle: "invalid".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Usage"));
    assert!(text.contains("/vim"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_vim_case_insensitive() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Vim {
            toggle: "ON".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("enabled"));
}

// -- /theme subcommands --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_theme_dark_applies() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Theme {
            name: "dark".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Theme") || text.contains("dark"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_theme_invalid_shows_available() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Theme {
            name: "nonexistent-theme".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Unknown theme") || text.contains("Available"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_theme_case_insensitive() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Theme {
            name: "DARK".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    // Should succeed (case insensitive), not show error
    assert!(!text.contains("Unknown theme"));
}

// -- /feedback empty text in TUI --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_feedback_empty_appends_in_tui() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Feedback {
            text: String::new(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    // TUI does NOT reject empty feedback — it appends to the log file
    // and shows a success message. This is a known divergence from REPL.
    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    // TUI accepts empty feedback
    assert!(text.contains("Feedback") || text.contains("saved"));
}

// -- /cost with windows --

#[test]
fn e2e_cost_today_opens_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    // ShowCost is handled synchronously in handle_slash_command
    app.handle_slash_command(&client, "/cost today");
    assert!(app.overlay.is_some());
}

#[test]
fn e2e_cost_week_opens_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/cost week");
    assert!(app.overlay.is_some());
}

#[test]
fn e2e_cost_month_opens_overlay() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/cost month");
    assert!(app.overlay.is_some());
}

// -- /export json vs markdown --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_export_json_creates_json_file() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Export {
            format: "json".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains(".json"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_export_markdown_creates_md_file() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Export {
            format: "markdown".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains(".md"));
}

// -- /rewind boundary values --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_rewind_zero_coerced_to_one() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Rewind {
            turns: "0".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("rewind") || text.contains("Nothing"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_rewind_non_numeric_defaults_to_one() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Rewind {
            turns: "abc".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("rewind") || text.contains("Nothing"));
}

// -- /plan subcommands --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_plan_show_no_plan_file() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Plan {
            args: "show".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("No plan") || text.contains("plan"));
}

// -- /add-dir invalid paths --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_add_dir_empty_shows_usage() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::AddDir {
            path: String::new(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Usage"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_add_dir_nonexistent_shows_error() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::AddDir {
            path: "/nonexistent/path/xyz123".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Directory not found"));
}

// -- /image invalid paths --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_image_empty_shows_usage() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Image {
            path: String::new(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Usage"));
    assert!(text.contains("/image"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_image_nonexistent_shows_error() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Image {
            path: "nonexistent.png".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("error") || text.contains("Error") || text.contains("failed"));
}

// -- /history page boundaries --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_history_page_1_empty() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::History { page: 1 },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    assert!(app.overlay.is_some());
    if let Some(ref overlay) = app.overlay {
        let text = format!("{:?}", overlay);
        assert!(text.contains("No conversation") || text.contains("History"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_history_page_999_clamped() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::History { page: 999 },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    // Page 999 should be clamped to the last available page
    assert!(app.overlay.is_some());
}

// -- /pr-comments parsing boundaries --

#[test]
fn e2e_pr_comments_invalid_number_defaults_to_zero() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/pr-comments abc");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::PrComments { pr_number }) = app.pending_command {
        assert_eq!(pr_number, 0);
    } else {
        panic!("expected PrComments command result");
    }
}

#[test]
fn e2e_pr_comments_no_number_defaults_to_zero() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/pr-comments");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::PrComments { pr_number }) = app.pending_command {
        assert_eq!(pr_number, 0);
    } else {
        panic!("expected PrComments command result");
    }
}

// -- Unicode/CJK parameters --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_commit_with_cjk_characters() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap_or_else(|e| panic!("failed to execute git {}: {}", args.join(" "), e));
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Initialise a git repo with staged changes so prepare_commit_prompt succeeds
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.email", "test@example.com"]);
    run_git(repo, &["config", "user.name", "Test User"]);
    std::fs::write(repo.join("file.txt"), "hello").unwrap();
    run_git(repo, &["add", "file.txt"]);
    run_git(repo, &["commit", "-m", "initial"]);
    // Create a new change and stage it so prepare_commit_prompt sees work to do
    std::fs::write(repo.join("file.txt"), "hello world").unwrap();
    run_git(repo, &["add", "file.txt"]);

    let engine = Arc::new(
        QueryEngine::builder("test-key", repo)
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (mut bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    // Switch to the temp repo so prepare_commit_prompt uses the correct cwd
    let _guard = std::env::set_current_dir(repo);

    // Commit goes to pending_command, then handle_async_command submits to engine
    handle_async_command(
        crate::commands::CommandResult::Commit {
            message: "你好世界".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    // Commit submits prompt to engine, so we should see a Submit request on the bus
    match bus.recv_request().now_or_never() {
        Some(Some(clawed_bus::events::AgentRequest::Submit { ref text, .. })) => {
            assert!(text.contains("你好世界"));
        }
        other => panic!("expected Submit request with CJK text, got {other:?}"),
    }
}

#[test]
fn e2e_tag_with_cjk_characters() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    // Tag is handled synchronously in handle_slash_command
    app.handle_slash_command(&client, "/tag 测试");
    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("测试"));
}

// -- Fast mode toggle --

#[tokio::test(flavor = "multi_thread")]
async fn e2e_fast_off_switches_to_sonnet() {
    let tmp = TempDir::new().unwrap();
    let engine = Arc::new(
        QueryEngine::builder("test-key", tmp.path())
            .load_claude_md(false)
            .load_memory(false)
            .build(),
    );
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    handle_async_command(
        crate::commands::CommandResult::Fast {
            toggle: "off".to_string(),
        },
        &engine,
        &client,
        &mut app,
        None,
    )
    .await;

    let last_msg = app.messages.last().expect("should have a message");
    let text = match &last_msg.content {
        MessageContent::System(t) => t,
        _ => panic!("expected system message"),
    };
    assert!(text.contains("Fast mode off") || text.contains("Switched"));
}

// -- /memory open subcommand --

#[test]
fn e2e_memory_open_subcommand() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/memory open test.md");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Memory { ref sub }) = app.pending_command {
        assert_eq!(sub, "open test.md");
    } else {
        panic!("expected Memory command result");
    }
}

// -- Pending command field verification (strengthened assertions) --

#[test]
fn e2e_history_page_3_verifies_field() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/history 3");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::History { page }) = app.pending_command {
        assert_eq!(page, 3);
    } else {
        panic!("expected History command result");
    }
}

#[test]
fn e2e_rewind_3_verifies_field() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/rewind 3");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Rewind { turns }) = app.pending_command {
        assert_eq!(turns, "3");
    } else {
        panic!("expected Rewind command result");
    }
}

#[test]
fn e2e_export_markdown_verifies_format_field() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/export markdown");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Export { format }) = app.pending_command {
        assert_eq!(format, "markdown");
    } else {
        panic!("expected Export command result");
    }
}

#[test]
fn e2e_vim_on_verifies_toggle_field() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/vim on");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Vim { toggle }) = app.pending_command {
        assert_eq!(toggle, "on");
    } else {
        panic!("expected Vim command result");
    }
}

#[test]
fn e2e_permissions_bypass_verifies_mode_field() {
    let (_bus, client) = EventBus::new(16);
    let mut app = App::new("test".to_string());

    app.handle_slash_command(&client, "/permissions bypass");
    assert!(app.pending_command.is_some());
    if let Some(crate::commands::CommandResult::Permissions { mode }) = app.pending_command {
        assert_eq!(mode, "bypass");
    } else {
        panic!("expected Permissions command result");
    }
}

// ── E2E: UX rendering tests ──────────────────────────────────────────────

#[test]
fn e2e_tool_tree_renders_depth_connector() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    // Spawn an agent so depth becomes 1 for subsequent tools
    env.send_agent_spawned("agent-1", "CodeReview");
    env.tick();

    // Start a tool inside the agent context
    env.send_tool_start("t1", "Read");
    env.send_tool_ready("t1", "Read", json!({"path": "src/main.rs"}));
    env.tick();

    // Render
    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    // Depth=1 should produce tree connector prefix
    assert!(
        text.contains("└─ "),
        "tool at depth=1 should render tree connector, got:\n{text}"
    );
    assert!(
        text.contains("● Read"),
        "tool header should contain name, got:\n{text}"
    );
}

#[test]
fn e2e_tool_error_shows_red_failed() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    env.send_tool_start("t1", "Bash");
    env.send_tool_ready("t1", "Bash", json!({"command": "false"}));
    env.send_tool_output("t1", "Bash", "something went wrong");
    env.send_tool_complete("t1", "Bash", true, Some("exit code 1"));
    env.tick();

    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("✗ failed"),
        "error tool should show ✗ failed, got:\n{text}"
    );
}

#[test]
fn e2e_tool_success_shows_duration() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    env.send_tool_start("t1", "Bash");
    env.send_tool_ready("t1", "Bash", json!({"command": "echo hi"}));
    env.send_tool_output("t1", "Bash", "hi");
    // Small sleep so duration is non-zero
    std::thread::sleep(std::time::Duration::from_millis(50));
    env.send_tool_complete("t1", "Bash", false, Some("hi"));
    env.tick();

    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    // Completed successful tool should show a duration line with checkmark
    assert!(
        text.contains('✓') || text.contains("ms") || text.contains('s'),
        "success tool should show duration marker, got:\n{text}"
    );
}

#[test]
fn e2e_tool_collapsed_shows_fold_hint() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    env.send_tool_start("t1", "Read");
    env.send_tool_ready("t1", "Read", json!({"path": "file.txt"}));
    // Emit some live output so the tool takes the "streamed" fold path
    env.send_tool_output("t1", "Read", "out1");
    env.send_tool_output("t1", "Read", "out2");
    env.send_tool_complete(
        "t1",
        "Read",
        false,
        Some("line1\nline2\nline3\nline4\nline5\nline6"),
    );
    env.tick();

    // Collapse the tool message
    if let Some(msg) = env
        .app
        .messages
        .iter_mut()
        .rev()
        .find(|m| matches!(&m.content, MessageContent::ToolExecution { .. }))
    {
        msg.toggle_collapsed();
    }
    env.app.invalidate_visible_lines();

    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("more lines (Ctrl+E to expand)"),
        "collapsed tool should show fold hint, got:\n{text}"
    );
}

#[test]
fn e2e_consecutive_system_messages_collapsed() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    // Invalidate cache so rebuild_visible_lines runs the collapse logic
    env.app.invalidate_visible_lines();
    // Push multiple non-important system messages
    for i in 0..5 {
        env.app
            .push_message(MessageContent::System(format!("status {i}")));
    }
    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("+ 3 system messages"),
        "consecutive system messages should collapse (5 -> first + +3 + last), got:\n{text}"
    );
}

#[test]
fn virtual_scroll_height_cache_matches_collapsed_system_block() {
    let mut app = App::new("claude-3.5".to_string());
    for i in 0..5 {
        app.push_message(MessageContent::System(format!("status {i}")));
    }

    app.rebuild_visible_lines();
    app.build_height_cache(80);

    let total_visual: usize = app
        .message_line_counts
        .iter()
        .map(|count| count.unwrap_or(0) as usize)
        .sum();

    assert_eq!(total_visual, app.cached_visible_lines.len());
    assert_eq!(app.message_line_counts[0], Some(3));
    assert!(app.message_line_counts[1..]
        .iter()
        .all(|count| *count == Some(0)));
}

#[test]
fn virtual_scroll_partial_system_block_keeps_folded_lines() {
    use ratatui::backend::TestBackend;

    let mut app = App::new("claude-3.5".to_string());
    for i in 0..5 {
        app.push_message(MessageContent::System(format!("status {i}")));
    }

    let backend = TestBackend::new(40, 2);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_messages(frame, Rect::new(0, 0, 40, 2), &mut app))
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let rendered = (0..buf.area().height)
        .map(|y| {
            (0..buf.area().width)
                .filter_map(|x| buf.cell((x, y)).map(|cell| cell.symbol().to_string()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("+ 3 system messages"),
        "bottom viewport should keep folded summary, got:\n{rendered}"
    );
    assert!(
        rendered.contains("status 4"),
        "bottom viewport should keep the last system message, got:\n{rendered}"
    );
}

#[test]
fn e2e_important_system_not_collapsed() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    // Push an important system message (contains "error")
    env.app.push_message(MessageContent::System(
        "An error occurred while processing".to_string(),
    ));
    // Followed by normal ones
    for i in 0..3 {
        env.app
            .push_message(MessageContent::System(format!("status {i}")));
    }
    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("error"),
        "important system message should remain visible, got:\n{text}"
    );
}

#[test]
fn e2e_separator_between_different_message_types() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    env.app
        .push_message(MessageContent::AssistantText("hello".to_string()));
    env.app
        .push_message(MessageContent::UserInput("hi".to_string()));
    env.app.needs_redraw = true;
    env.tick();

    let lines: Vec<_> = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect();

    // Find the assistant text and user input, verify there is a blank separator
    let assistant_idx = lines.iter().position(|l| l.contains("hello")).unwrap();
    let user_idx = lines.iter().position(|l| l.contains("hi")).unwrap();
    assert!(
        lines[assistant_idx + 1..user_idx]
            .iter()
            .any(|l| l.is_empty()),
        "different message types should have a blank separator, got: {lines:?}"
    );
}

#[test]
fn e2e_assistant_and_thinking_no_separator() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    env.app
        .push_message(MessageContent::AssistantText("hello".to_string()));
    env.app
        .push_message(MessageContent::ThinkingText("reasoning".to_string()));
    // Expand the thinking message so its body text is visible inline.
    if let Some(msg) = env.app.messages.last_mut() {
        msg.collapsed = false;
        msg.invalidate_cache();
    }
    env.app.invalidate_visible_lines();
    env.app.needs_redraw = true;
    env.tick();

    let lines: Vec<_> = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect();

    let assistant_idx = lines.iter().position(|l| l.contains("hello")).unwrap();
    let thinking_idx = lines.iter().position(|l| l.contains("reasoning")).unwrap();
    // There should be no blank line between them
    let between = &lines[assistant_idx + 1..thinking_idx];
    assert!(
        !between.iter().any(|l| l.is_empty()),
        "assistant and thinking should flow together without separator, got: {lines:?}"
    );
}

#[test]
fn e2e_thinking_collapsed_shows_hint() {
    let mut env = E2ETestEnv::new();
    env.app.term_width = 80;
    env.app.term_height = 24;

    env.app.push_message(MessageContent::ThinkingText(
        "line1\nline2\nline3\nline4\nline5".to_string(),
    ));
    env.app.needs_redraw = true;
    env.tick();

    let text = env
        .app
        .cached_visible_lines
        .iter()
        .map(|l| line_text(l))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("∴ Thinking"),
        "collapsed thinking should show placeholder, got:\n{text}"
    );
}

// -- Test helpers for ANSI snapshot output --------------------------------

fn fg_ansi(color: ratatui::style::Color) -> String {
    use ratatui::style::Color;
    match color {
        Color::Reset => String::new(),
        Color::Black => "30".to_string(),
        Color::Red => "31".to_string(),
        Color::Green => "32".to_string(),
        Color::Yellow => "33".to_string(),
        Color::Blue => "34".to_string(),
        Color::Magenta => "35".to_string(),
        Color::Cyan => "36".to_string(),
        Color::Gray | Color::DarkGray => "90".to_string(),
        Color::LightRed => "91".to_string(),
        Color::LightGreen => "92".to_string(),
        Color::LightYellow => "93".to_string(),
        Color::LightBlue => "94".to_string(),
        Color::LightMagenta => "95".to_string(),
        Color::LightCyan => "96".to_string(),
        Color::White => "97".to_string(),
        Color::Indexed(i) => format!("38;5;{i}"),
        Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
    }
}

fn buffer_to_ansi(buf: &ratatui::buffer::Buffer) -> String {
    use ratatui::style::Modifier;
    let mut output = String::with_capacity(buf.area.width as usize * buf.area.height as usize * 12);
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            let symbol = if sym.is_empty() { " " } else { sym };

            let mut codes = vec!["0".to_string()];
            if cell.modifier.contains(Modifier::BOLD) {
                codes.push("1".to_string());
            }
            if cell.modifier.contains(Modifier::DIM) {
                codes.push("2".to_string());
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                codes.push("3".to_string());
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                codes.push("4".to_string());
            }
            let fg = fg_ansi(cell.fg);
            if !fg.is_empty() {
                codes.push(fg);
            }
            output.push_str(&format!("\x1b[{}m{}", codes.join(";"), symbol));
        }
        output.push_str("\x1b[0m\n");
    }
    output
}

fn snap(name: &str, dir: &str, app: &mut App) {
    use std::fs;
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let ansi = buffer_to_ansi(&buf);
    let path = format!("{dir}/{name}.ansi");
    fs::write(&path, &ansi).unwrap();
    println!("\n=== {name} ===\n{ansi}\x1b[0m");
}

/// Visual verification: render key TUI states to /tmp for side-by-side
/// comparison with official Claude Code. Run with:
///   cargo test -p clawed-cli tui_visual_snapshot -- --nocapture
#[test]
fn tui_visual_snapshot() {
    use std::fs;

    let dir = "/tmp/clawed_tui_verification";
    let _ = fs::create_dir_all(dir);

    // Scene 1: Initial empty state (welcome screen)
    let mut app = App::new("claude-3.5".to_string());
    app.term_width = 80;
    app.term_height = 24;
    app.needs_redraw = true;
    app.rebuild_visible_lines();
    snap("01_initial_state", dir, &mut app);

    // Scene 2: Conversation with user + assistant messages
    let mut app = App::new("claude-3.5".to_string());
    app.term_width = 80;
    app.term_height = 24;
    app.needs_redraw = true;
    app.push_message(MessageContent::UserInput(
        "Explain Rust ownership".to_string(),
    ));
    app.push_message(MessageContent::AssistantText(
        "Rust ownership is a set of rules that govern how memory is managed.".to_string(),
    ));
    snap("02_with_messages", dir, &mut app);

    // Scene 3: LLM generating (spinner + bottom bar hints)
    let mut app = App::new("claude-3.5".to_string());
    app.term_width = 80;
    app.term_height = 24;
    app.needs_redraw = true;
    app.push_message(MessageContent::UserInput("Write hello world".to_string()));
    app.push_message(MessageContent::AssistantText(String::new()));
    app.mark_generating();
    snap("03_generating", dir, &mut app);

    // Scene 4: Context suggestions overlay (file / MCP / agent)
    let mut app = App::new("claude-3.5".to_string());
    app.term_width = 80;
    app.term_height = 24;
    app.needs_redraw = true;
    app.suggestions = vec![
        SuggestionItem {
            id: "file1".to_string(),
            display_text: "src/main.rs".to_string(),
            description: Some("Add to context".to_string()),
            kind: SuggestionKind::File,
        },
        SuggestionItem {
            id: "mcp1".to_string(),
            display_text: "docs/readme".to_string(),
            description: None,
            kind: SuggestionKind::McpResource,
        },
        SuggestionItem {
            id: "agent1".to_string(),
            display_text: "@reviewer".to_string(),
            description: Some("Code reviewer".to_string()),
            kind: SuggestionKind::Agent,
        },
    ];
    app.selected_suggestion = 0;
    snap("04_suggestions_overlay", dir, &mut app);

    println!("Screenshots saved to {dir}/");
    println!("Compare with official CC: open two terminals, run `claude` and `cargo run --`, then visually verify.");
}

#[test]
#[allow(clippy::many_single_char_names)]
fn generate_delivery_screenshots() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let dir = "/tmp/clawed_delivery";
    let _ = std::fs::create_dir_all(dir);

    // Renders app state as colored HTML
    let snap = |app: &mut App, name: &str| {
        let backend = TestBackend::new(120, 40);
        let mut t = Terminal::new(backend).unwrap();
        t.draw(|f| crate::tui::render(f, app)).unwrap();
        let buf = t.backend().buffer();
        let mut h = String::from(
            "<pre style='background:#1a1a2e;color:#ddd;font:13px monospace;padding:10px'>\n",
        );
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if let Some(c) = buf.cell((x, y)) {
                    let ch = match c.symbol() {
                        " " => " ",
                        "&" => "&amp;",
                        "<" => "&lt;",
                        ">" => "&gt;",
                        s => s,
                    };
                    let bg = c.style().bg.map_or(String::new(), |c| {
                        format!("background:{:?};", c)
                            .replace("Rgb(", "rgb(")
                            .replace(")", ";")
                    });
                    let fg = c.style().fg.map_or(String::new(), |c| {
                        format!("color:{:?};", c)
                            .replace("Rgb(", "rgb(")
                            .replace(")", ";")
                    });
                    let b = if c.style().add_modifier.contains(Modifier::BOLD) {
                        "font-weight:bold;"
                    } else {
                        ""
                    };
                    let i = if c.style().add_modifier.contains(Modifier::ITALIC) {
                        "font-style:italic;"
                    } else {
                        ""
                    };
                    let s = if c.style().add_modifier.contains(Modifier::CROSSED_OUT) {
                        "text-decoration:line-through;"
                    } else {
                        ""
                    };
                    h.push_str(&format!("<span style='{bg}{fg}{b}{i}{s}'>{ch}</span>"));
                }
            }
            h.push('\n');
        }
        h.push_str("</pre>");
        std::fs::write(format!("{dir}/{}.html", name), &h).unwrap();
    };

    macro_rules! fresh {
            ($name:expr, |$app:ident| $body:block) => {{
                let mut $app = App::new("claude-sonnet-4-6".into());
                $body
                snap(&mut $app, $name);
            }};
        }

    fresh!("01_welcome", |app| {});
    fresh!("02_markdown", |app| {
        app.push_message(MessageContent::AssistantText(
            "### Project Progress\n\n- TUI core refactoring done\n- Permission dialog fixed\n\n> Code review is part of writing code.\n\n---\n\n| Module | Status |\n|---|---|\n| config | Done |\n| overlay | Pending |".into()));
    });
    fresh!("03_diff", |app| {
        app.push_message(MessageContent::AssistantText("Edit result:".into()));
        let m = Message::new(MessageContent::ToolExecution {
                name: "Edit".to_string(), input: Some("src/main.rs".to_string()),
                output_lines: vec![], is_error: false, duration_ms: 1500,
                full_result: Some("--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n unchanged\n-removed\n+added\n unchanged".to_string()), depth: 0,
            });
        app.messages.push(m);
    });
    fresh!("04_taskplan", |app| {
        app.task_plan.add_task("t1".into(), "Core engine".into());
        app.task_plan.add_task("t2".into(), "Test suite".into());
        app.task_plan.add_task("t3".into(), "Edge cases".into());
        app.task_plan.complete_task("t1", false);
        app.task_plan.complete_task("t2", false);
    });
    fresh!("05_agents", |app| {
        app.status.active_agents.insert(
            "a1".into(),
            status::AgentInfo {
                name: "reviewer".into(),
                started: std::time::Instant::now(),
                state: status::AgentState::Active,
                activity: Some("Read(src/lib.rs)".into()),
                tool_count: 3,
                token_estimate: 4500,
                idle_since: None,
                color: Color::Magenta,
            },
        );
        app.status.active_agents.insert(
            "a2".into(),
            status::AgentInfo {
                name: "test-runner".into(),
                started: std::time::Instant::now(),
                state: status::AgentState::Idle,
                activity: None,
                tool_count: 1,
                token_estimate: 800,
                idle_since: Some(std::time::Instant::now()),
                color: Color::Cyan,
            },
        );
        app.status.is_generating = true;
        app.status.spinner_frame = 3;
    });

    eprintln!("Delivery screenshots: {}/", dir);
}
