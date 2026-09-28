#![allow(clippy::field_reassign_with_default)]
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};
use sterna::contract::{Conversation, Message, Role, ServedBy};
use sterna::helpers::{HelperOutcome, HelperRecord};
use sterna::runtime::handles::HandleTable;
use sterna::tui::{
    Activity, CellError, CellView, ContextTokens, Counted, Notebook, ScreenState,
    SidebarVisibility, render_screen, screen_regions, slash_matches,
};

fn state() -> ScreenState {
    ScreenState {
        model: Some("claude-fable-5.1".into()),
        project: Some("glasshouse".into()),
        sandbox: Some("3p/4c".into()),
        network: Some("off".into()),
        connected: Some(true),
        ..ScreenState::default()
    }
}
fn conversation() -> Conversation {
    Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Inspect the request path."),
            Message::text(Role::Assistant, "I found the gateway configuration."),
        ],
    }
}
fn draw(
    width: u16,
    height: u16,
    state: &ScreenState,
    conversation: &Conversation,
    notebook: &Notebook,
) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            render_screen(
                frame,
                conversation,
                &ServedBy::default(),
                &HandleTable::new(),
                notebook,
                state,
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}
fn text(buffer: &Buffer) -> String {
    (buffer.area.y..buffer.area.bottom())
        .map(|y| {
            (buffer.area.x..buffer.area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
/// The first row of the answer block, found by its header.
fn answer_row(buffer: &Buffer, regions: &sterna::tui::ScreenRegions) -> u16 {
    let rendered = text(buffer);
    let at = rendered
        .lines()
        .position(|line| line.contains(" STERNA") && !line.contains("STERNA /"))
        .expect("the fixture has an answer");
    let _ = regions;
    u16::try_from(at).unwrap()
}
fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
}

/// The request is framed and the answer is tinted — the two things a reader
/// scrolling a long transcript navigates by. Before 2026-09-18 both were a
/// bare word on a transparent row and a turn boundary was an empty line, so
/// the screen was one column of text with nothing marking where anything
/// began (user, from a screenshot: "there is nothing at all").
#[test]
fn a_turn_block_has_a_visible_boundary_and_a_header() {
    let buffer = draw(80, 30, &state(), &conversation(), &Notebook::default());
    let rendered = text(&buffer);
    let lines: Vec<_> = rendered.lines().collect();
    let at = |header: &str| {
        lines
            .iter()
            .position(|line| line.contains(header) && !line.contains("STERNA /"))
            .unwrap()
    };

    // The request: a rule that reaches the edge of the transcript, in the
    // theme's accent, and no fill -- a frame, so a transparent terminal
    // stays transparent behind the person's own words.
    let user = at("USER");
    assert!(
        lines[user].trim_end().ends_with('━'),
        "the request's rule reaches the edge: {:?}",
        lines[user]
    );
    assert_eq!(buffer[(0, user as u16)].bg, ratatui::style::Color::Reset);
    assert_eq!(buffer[(0, user as u16)].fg, state().theme.accent());
    assert!(lines[user + 1].contains("Inspect"));
    assert!(lines[user + 1].starts_with(' '));

    // The answer: a muted ground the block sits on, header and body alike.
    let sterna = at(" STERNA");
    let hush = buffer[(0, sterna as u16)].bg;
    assert_ne!(hush, ratatui::style::Color::Reset, "the answer is tinted");
    assert!(lines[sterna + 1].contains("I found"));
    assert!(lines[sterna + 1].starts_with(' '));
    assert_eq!(
        buffer[(0, sterna as u16 + 1)].bg,
        hush,
        "the whole block is tinted, not only its header"
    );
}

#[test]
fn no_region_writes_outside_its_own_rect_at_60_80_120_and_200_columns() {
    for width in [60, 80, 120, 200] {
        let mut state = state();
        state.input = "Ж".repeat(300);
        state.model = Some("Ф".repeat(100));
        state.project = Some("Ю".repeat(100));
        let conversation = Conversation {
            system: String::new(),
            messages: vec![
                Message::text(Role::User, "Щ".repeat(100)),
                Message::text(Role::Assistant, "Ц".repeat(100)),
            ],
        };
        let notebook = Notebook {
            cells: vec![CellView {
                table: Some("Б".repeat(100)),
                error: Some(CellError {
                    class: "Error".into(),
                    message: "Д".repeat(100),
                    ..CellError::default()
                }),
                ..CellView::default()
            }],
            ..Notebook::default()
        };
        let buffer = draw(width, 70, &state, &conversation, &notebook);
        let r = screen_regions(buffer.area, &state);
        let rects = [
            r.header,
            r.transcript,
            r.details,
            r.completions,
            r.input,
            r.status,
        ];
        for y in 0..buffer.area.height {
            for x in 0..width {
                assert!(rects.iter().filter(|rect| contains(**rect, x, y)).count() <= 1);
                for glyph in buffer[(x, y)].symbol().chars() {
                    let valid = match glyph {
                        'Ж' => contains(r.input, x, y),
                        'Ф' => contains(r.status, x, y),
                        'Ю' => contains(r.header, x, y) || contains(r.status, x, y),
                        'Щ' | 'Ц' | 'Б' | 'Д' => contains(r.transcript, x, y),
                        'Э' => contains(r.details, x, y),
                        _ => true,
                    };
                    assert!(valid, "{glyph} escaped at {width}: {x},{y}");
                }
            }
        }
        let rendered = text(&buffer);
        for glyph in ['Ж', 'Ф', 'Ю', 'Щ', 'Ц', 'Б', 'Д'] {
            assert!(rendered.contains(glyph), "missing {glyph} at {width}");
        }
        // Every body starts intact after the rail, including the first column.
        for glyph in ['Щ', 'Ц', 'Б'] {
            assert!(
                rendered
                    .lines()
                    .any(|line| line.starts_with(&format!(" {glyph}")))
            );
        }
        let at: Vec<_> = ['Щ', 'Ц', 'Б', 'Д']
            .into_iter()
            .map(|c| rendered.find(c).unwrap())
            .collect();
        assert!(
            at.windows(2).all(|pair| pair[0] < pair[1]),
            "turns interleaved"
        );
    }
}

#[test]
fn the_status_line_names_the_model_the_project_the_sandbox_and_the_connection() {
    for width in [60, 80, 120, 200] {
        let rendered = text(&draw(
            width,
            24,
            &state(),
            &conversation(),
            &Notebook::default(),
        ));
        for word in [
            "claude-fable-5.1",
            "glasshouse",
            "sandbox 3p/4c",
            "net:off",
            "gateway connected",
        ] {
            assert!(rendered.contains(word), "{word}: {rendered}");
        }
    }
}
/// The sandbox level is on the status line, beside the effort, at every
/// width this test draws — a person on `full` must not be able to forget it,
/// and the sidebar they can hide is not where that belongs.
#[test]
fn the_status_line_names_the_sandbox_level_beside_the_effort() {
    for level in sterna::permissions::Level::ALL {
        let mut state = state();
        state.level = sterna::permissions::LiveLevel::new(level);
        for width in [80, 120, 200] {
            let rendered = text(&draw(
                width,
                24,
                &state,
                &conversation(),
                &Notebook::default(),
            ));
            assert!(
                rendered.contains(&format!("{} · effort ", level.name())),
                "the level `{}` beside the effort at width {width}: {rendered}",
                level.name()
            );
        }
    }
}

#[test]
fn gateway_provenance_does_not_invent_a_billing_status() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let served = ServedBy {
        provider: Some("subscription-broker".into()),
        ..ServedBy::default()
    };
    let mut state = state();
    state.connected = None;
    terminal
        .draw(|frame| {
            render_screen(
                frame,
                &conversation(),
                &served,
                &HandleTable::new(),
                &Notebook::default(),
                &state,
            )
        })
        .unwrap();
    let rendered = text(terminal.backend().buffer());
    assert!(rendered.contains("gateway routed"));
    assert!(!rendered.contains("metered"));
}

#[test]
fn the_input_area_shows_what_is_being_composed_and_is_separated_from_the_transcript() {
    let mut state = state();
    state.input = "first line\nsecond line".into();
    let buffer = draw(60, 24, &state, &conversation(), &Notebook::default());
    let r = screen_regions(buffer.area, &state);
    assert!(text(&buffer).contains("first line"));
    assert!(text(&buffer).contains("second line"));
    let rendered = text(&buffer);
    let first = rendered
        .lines()
        .position(|line| line.contains("first line"))
        .unwrap();
    let second = rendered
        .lines()
        .position(|line| line.contains("second line"))
        .unwrap();
    assert_eq!(second, first + 1);
    assert!(
        text(&buffer)
            .lines()
            .nth(usize::from(r.input.y))
            .unwrap()
            .contains("───")
    );
    assert!(r.transcript.bottom() <= r.input.y && r.input.bottom() <= r.status.y);
}
#[test]
fn slash_completion_uses_real_commands_and_filters_as_letters_arrive() {
    // The workbench added `/diff`, `/activity` and `/subagents`; the
    // discovery pass added `/tool`, `/login` and `/mouse`, which worked and
    // were in no list, and removed a second `/config` that had been listed
    // twice with two different descriptions. `/usage` (2026-09-23) shows
    // each subscription's limits. `/setup` (2026-09-25) is the first-start
    // wizard. `/bird` (2026-09-23) went on 2026-09-26: the parrots are themes.
    // `/stream` worked and was in no list until the settings pass. `/resume`
    // (2026-09-27) opens the resume sheet from inside a session.
    // `/supervisor` went with the supervisor on 2026-09-28, and with it
    // `/budget`, `/usage` (the Session sheet shows the limits), `/handles`,
    // `/entitlements`, `/activity`, `/sidebar` (Ctrl-B) and `/handlers`.
    assert_eq!(slash_matches("/").len(), 30);
    let offered = slash_matches("/");
    let mut unique: Vec<&str> = offered.iter().map(|(n, _)| n.as_str()).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(offered.len(), unique.len(), "no command is offered twice");
    for command in ["/diff", "/subagents", "/tool", "/login", "/mouse"] {
        assert!(
            slash_matches("/").iter().any(|(name, _)| name == command),
            "{command} is offered"
        );
    }
    assert!(
        slash_matches("/se")
            .iter()
            .any(|(name, _)| name == "/settings")
    );
    assert!(
        slash_matches("/co")
            .iter()
            .any(|(name, _)| name == "/config")
    );
    assert!(
        slash_matches("/ex").iter().any(|(name, _)| name == "/exit"),
        "the command that ends the session must be offered by the menu"
    );
    assert_eq!(
        slash_matches("/mo"),
        vec![
            ("/model".into(), "set the parent, helper or subagent model"),
            (
                "/models".into(),
                "browse models by agent, provider or intelligence"
            ),
            ("/motion".into(), "full, calm or off · how much moves"),
            ("/mouse".into(), "release or recapture the mouse · Ctrl-G")
        ]
    );
    assert!(slash_matches("/sa").contains(&(
        "/sandbox".into(),
        "how much runs without asking · Ask, Sandboxed or Full access"
    )));
    assert!(slash_matches("/pl").contains(&(
        "/plan".into(),
        "plan one request · it reads, and writes only the plan"
    )));
    for input in ["hello", "/model something", "/unknown", "/permissions"] {
        assert!(slash_matches(input).is_empty());
    }
    let mut state = state();
    state.input = "/mo".into();
    let buffer = draw(60, 24, &state, &conversation(), &Notebook::default());
    let r = screen_regions(buffer.area, &state);
    let rendered = text(&buffer);
    assert!(
        rendered
            .lines()
            .nth(usize::from(r.completions.y))
            .unwrap()
            .contains("/model")
    );
    assert!(rendered.contains("set the parent, helper or subagent model"));
}
#[test]
fn the_root_view_does_not_draw_an_outer_window_border() {
    for width in [60, 80, 120, 200] {
        let buffer = draw(width, 24, &state(), &conversation(), &Notebook::default());
        assert_ne!(buffer[(0, 0)].symbol(), "┌");
        assert_ne!(buffer[(width - 1, 23)].symbol(), "┘");
        assert!(!text(&buffer).contains("Conversation"));
    }
}
#[test]
fn narrow_layout_collapses_secondary_regions_instead_of_overlapping_them() {
    for width in [60, 80] {
        assert_eq!(
            screen_regions(Rect::new(0, 0, width, 24), &state())
                .details
                .width,
            0
        );
    }
    for width in [120, 200] {
        assert_eq!(
            screen_regions(Rect::new(0, 0, width, 24), &state())
                .transcript
                .width,
            width - 36
        );
    }
}
#[test]
fn wide_telemetry_preserves_reported_fields_and_budget_provenance() {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let served = ServedBy {
        provider: Some("anthropic".into()),
        model: Some("claude-fable-5.1".into()),
        route: Some("direct".into()),
        quota_context: Some("subscription".into()),
        input_tokens: Some(123),
        output_tokens: Some(456),
        cached_input_tokens: Some(100),
    };
    let notebook = Notebook {
        context: Some(ContextTokens {
            used: 44_584,
            cap: Some(1_048_576),
            cap_source: sterna::models::WindowSource::Observed,
            counted: Counted::Gateway,
        }),
        tokens: Some(sterna::tui::TaskTokens {
            used: 579,
            parent_used: 579,
            helpers: Default::default(),
            counted: sterna::tui::Counted::Gateway,
        }),
        ..Notebook::default()
    };
    terminal
        .draw(|frame| {
            render_screen(
                frame,
                &conversation(),
                &served,
                &HandleTable::new(),
                &notebook,
                &state(),
            )
        })
        .unwrap();
    let rendered = text(terminal.backend().buffer());
    for field in [
        "anthropic",
        "subscription",
        "route: direct",
        "cached input: 100",
        "tokens: 123 in / 456 out",
        "Σ 579 tokens",
        "ctx",
        "44.6k/1.0M 4%",
    ] {
        assert!(rendered.contains(field), "{field}: {rendered}");
    }
    // **Provenance is preserved by being made exceptional.** `reported` was
    // printed on every session, which is what made `estimated` invisible; a
    // gateway-counted total now says nothing and an estimated one speaks.
    assert!(
        !rendered.contains("counted: reported"),
        "the healthy provenance must not caption every session: {rendered}"
    );
    assert!(
        !rendered.contains("cumulative task spend · no cap"),
        "a caption that is always true is not a signal: {rendered}"
    );
}

#[test]
fn telemetry_breaks_exact_task_spend_down_by_parent_helper_model_and_cache() {
    let notebook = Notebook {
        tokens: Some(sterna::tui::TaskTokens {
            used: 201_801,
            parent_used: 173_860,
            helpers: sterna::tui::HelperTokens {
                calls: 3,
                usage_known_calls: 3,
                used: 27_941,
                input_tokens: 20_329,
                output_tokens: 2_492,
                requests: 6,
                reported_requests: 6,
                cache_read_input_tokens: 5_120,
                cache_creation_input_tokens: 0,
                cache_read_reported_requests: 6,
                cache_creation_reported_requests: 6,
                models: vec![sterna::tui::HelperModelTokens {
                    model: "gpt-5.6-luna".into(),
                    calls: 3,
                    usage_known_calls: 3,
                    used: 27_941,
                    input_tokens: 20_329,
                    output_tokens: 2_492,
                    requests: 6,
                    reported_requests: 6,
                    cache_read_input_tokens: 5_120,
                    cache_creation_input_tokens: 0,
                    cache_read_reported_requests: 6,
                    cache_creation_reported_requests: 6,
                }],
            },
            counted: Counted::Gateway,
        }),
        ..Notebook::default()
    };

    // The glance and the detail are two surfaces now. The rail carries the
    // total and the split a reader asked for; the per-model arithmetic lives
    // behind Ctrl-T, where somebody went looking for it.
    let rail = text(&draw(120, 40, &state(), &conversation(), &notebook));
    for field in ["Σ 201.8k tokens", "parent 173.9k · helpers 27.9k"] {
        assert!(rail.contains(field), "{field}: {rail}");
    }
    assert!(
        rail.contains("gpt-5.6-luna"),
        "the one helper model folds onto the split: {rail}"
    );

    let mut open = state();
    open.telemetry_open = true;
    let expanded = text(&draw(120, 40, &open, &conversation(), &notebook));
    for field in [
        "Σ 201.8k tokens",
        "in 20.3k · out 2.5k",
        "cache read 5.1k",
        "cache create 0",
        "3 of 3 calls counted",
    ] {
        assert!(expanded.contains(field), "{field}: {expanded}");
    }
    assert!(!expanded.contains("helper usage partial"), "{expanded}");
}

#[test]
fn telemetry_calls_missing_cache_classes_unreported_instead_of_zero() {
    let notebook = Notebook {
        tokens: Some(sterna::tui::TaskTokens {
            used: 173_875,
            parent_used: 173_860,
            helpers: sterna::tui::HelperTokens {
                calls: 1,
                usage_known_calls: 1,
                used: 15,
                input_tokens: 10,
                output_tokens: 5,
                requests: 1,
                reported_requests: 1,
                models: vec![sterna::tui::HelperModelTokens {
                    model: "helper-model".into(),
                    calls: 1,
                    usage_known_calls: 1,
                    used: 15,
                    input_tokens: 10,
                    output_tokens: 5,
                    requests: 1,
                    reported_requests: 1,
                    ..sterna::tui::HelperModelTokens::default()
                }],
                ..sterna::tui::HelperTokens::default()
            },
            counted: Counted::Gateway,
        }),
        ..Notebook::default()
    };

    // `unreported` is not `0`, and the instruments are where that is said.
    let mut open = state();
    open.telemetry_open = true;
    let expanded = text(&draw(120, 40, &open, &conversation(), &notebook));
    assert!(expanded.contains("cache read unreported"), "{expanded}");
    assert!(expanded.contains("cache create unreported"), "{expanded}");
    assert!(expanded.contains("1 of 1 calls counted"), "{expanded}");
    // The rail says the same thing in one character rather than three lines:
    // the total is a known-low subtotal, so it carries `+`.
    let rail = text(&draw(120, 40, &state(), &conversation(), &notebook));
    assert!(
        rail.contains("173.9k+") || rail.contains("+ tokens"),
        "an incomplete count must mark the total: {rail}"
    );
}

#[test]
fn statusline_separates_request_context_from_cumulative_spend() {
    let notebook = Notebook {
        context: Some(ContextTokens {
            used: 44_584,
            cap: Some(1_048_576),
            cap_source: sterna::models::WindowSource::Observed,
            counted: Counted::Gateway,
        }),
        tokens: Some(sterna::tui::TaskTokens {
            used: 369_178,
            parent_used: 369_178,
            helpers: Default::default(),
            counted: Counted::Gateway,
        }),
        ..Notebook::default()
    };
    let shown = text(&draw(200, 30, &state(), &conversation(), &notebook));
    assert!(shown.contains("ctx"), "{shown}");
    assert!(shown.contains("44.6k/1.0M 4%"), "{shown}");
    assert!(shown.contains("spent 369.2k · reported"), "{shown}");

    let unknown = Notebook {
        context: Some(ContextTokens {
            used: 44_584,
            cap: None,
            cap_source: sterna::models::WindowSource::Observed,

            counted: Counted::Gateway,
        }),
        ..notebook
    };
    let shown = text(&draw(200, 30, &state(), &conversation(), &unknown));
    assert!(
        shown.contains("context 44.6k tokens · provider count") && !shown.contains("window ?"),
        "{shown}"
    );
    assert!(!shown.contains("44.6k/400.0k"), "{shown}");
}

#[test]
fn context_fill_animates_while_busy_without_changing_its_measurement() {
    let notebook = Notebook {
        context: Some(ContextTokens {
            used: 600_000,
            cap: Some(1_000_000),
            cap_source: sterna::models::WindowSource::Observed,

            counted: Counted::Estimated,
        }),
        ..Notebook::default()
    };
    let mut first_state = state();
    first_state.activity = Activity::Thinking;
    let first = draw(200, 30, &first_state, &conversation(), &notebook);
    first_state.animation_frame = 1;
    let moved = draw(200, 30, &first_state, &conversation(), &notebook);
    let status = screen_regions(first.area, &first_state).status;
    assert!(
        (status.y..status.bottom())
            .any(|y| { (status.x..status.right()).any(|x| first[(x, y)] != moved[(x, y)]) }),
        "the measured fill should carry a moving glint"
    );
    let first_text = text(&first);
    let moved_text = text(&moved);
    for measurement in ["600.0k/1.0M 60%", "spent"] {
        assert_eq!(
            first_text.contains(measurement),
            moved_text.contains(measurement)
        );
    }
}

#[test]
fn the_sidebar_can_be_hidden_and_the_preference_survives_resize() {
    let mut state = state();
    // The rail's task spend is the sentinel: it is drawn only there.
    let notebook = Notebook {
        tokens: Some(sterna::tui::TaskTokens {
            used: 579,
            parent_used: 579,
            helpers: Default::default(),
            counted: sterna::tui::Counted::Gateway,
        }),
        ..Notebook::default()
    };
    assert!(text(&draw(120, 24, &state, &conversation(), &notebook)).contains("Σ 579 tokens"));
    state.sidebar = SidebarVisibility::Hidden;
    for width in [200, 60, 120] {
        let buffer = draw(width, 24, &state, &conversation(), &notebook);
        assert_eq!(screen_regions(buffer.area, &state).details.width, 0);
        assert!(!text(&buffer).contains("Σ 579 tokens"));
        assert!(text(&buffer).contains("gateway connected"));
    }
    state.sidebar = SidebarVisibility::Auto;
    assert!(text(&draw(120, 24, &state, &conversation(), &notebook)).contains("Σ 579 tokens"));
}

#[test]
fn sidebar_width_is_fixed_and_explicit_visibility_works_at_80_columns() {
    let mut state = state();
    for width in [120, 200, 300] {
        let r = screen_regions(Rect::new(5, 3, width, 30), &state);
        assert_eq!(r.details.width, 34);
        assert_eq!(r.details.right(), 5 + width);
        assert!(r.details.x >= r.transcript.right() + 2);
    }
    assert_eq!(
        screen_regions(Rect::new(0, 0, 80, 30), &state)
            .details
            .width,
        0
    );
    state.sidebar = SidebarVisibility::Shown;
    assert_eq!(
        screen_regions(Rect::new(0, 0, 80, 30), &state)
            .details
            .width,
        34
    );
    assert_eq!(
        screen_regions(Rect::new(0, 0, 60, 30), &state)
            .details
            .width,
        0
    );
}

#[test]
fn animated_indicator_frames_have_constant_bounds_and_history_is_stable() {
    for activity in [
        Activity::Starting,
        Activity::Thinking,
        Activity::Streaming,
        Activity::Executing,
        Activity::Searching,
        Activity::Waiting,
        Activity::Compacting,
        Activity::Complete,
        Activity::Failed,
    ] {
        let mut state = state();
        state.activity = activity;
        let first = draw(60, 24, &state, &conversation(), &Notebook::default());
        for tick in 0..12 {
            assert_eq!(activity.indicator(tick).len(), 4);
            state.animation_frame = tick;
            let next = draw(60, 24, &state, &conversation(), &Notebook::default());
            let r = screen_regions(next.area, &state);
            for y in r.transcript.y..r.transcript.bottom() {
                for x in 0..60 {
                    assert_eq!(first[(x, y)], next[(x, y)]);
                }
            }
            if [Activity::Complete, Activity::Failed].contains(&activity) {
                assert_eq!(first, next);
            }
        }
    }
}
#[test]
fn a_fresh_terminal_over_existing_output_erases_every_old_cell() {
    use ratatui::backend::Backend;
    use ratatui::buffer::Cell;
    for width in [60, 80, 120, 200] {
        let mut backend = TestBackend::new(width, 30);
        let old_cell = Cell::new("Z");
        let old = &old_cell;
        backend
            .draw((0..30).flat_map(|y| (0..width).map(move |x| (x, y, old))))
            .unwrap();
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_screen(
                    frame,
                    &conversation(),
                    &ServedBy::default(),
                    &HandleTable::new(),
                    &Notebook::default(),
                    &state(),
                )
            })
            .unwrap();
        assert_eq!(
            terminal.backend().buffer(),
            &draw(width, 30, &state(), &conversation(), &Notebook::default()),
            "old stdout survived a new Terminal at {width}"
        );
    }
}

#[test]
fn resizing_clears_stale_cells_and_tiny_terminals_do_not_panic() {
    let mut terminal = Terminal::new(TestBackend::new(200, 30)).unwrap();
    let mut state = state();
    state.input = "/".into();
    for (width, height) in [
        (200, 30),
        (60, 24),
        (120, 30),
        (80, 24),
        (1, 1),
        (2, 3),
        (0, 0),
    ] {
        terminal.backend_mut().resize(width, height);
        terminal.autoresize().unwrap();
        terminal
            .draw(|frame| {
                render_screen(
                    frame,
                    &conversation(),
                    &ServedBy::default(),
                    &HandleTable::new(),
                    &Notebook::default(),
                    &state,
                )
            })
            .unwrap();
        assert_eq!(
            terminal.backend().buffer(),
            &draw(width, height, &state, &conversation(), &Notebook::default())
        );
    }
}
#[test]
fn long_unicode_transcripts_follow_the_tail_without_losing_the_newest_turn() {
    let mut conversation = conversation();
    conversation
        .messages
        .insert(0, Message::text(Role::User, "界 e\u{301} ".repeat(2000)));
    conversation
        .messages
        .push(Message::text(Role::User, "LAST TURN INTACT"));
    let rendered = text(&draw(60, 24, &state(), &conversation, &Notebook::default()));
    assert!(rendered.contains("LAST TURN INTACT"));
}
#[test]
fn visual_review_captures() {
    let Some(dir) = std::env::var_os("STERNA_LOOK_CAPTURES") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    for width in [60, 80, 120, 200] {
        for activity in [
            Activity::Starting,
            Activity::Thinking,
            Activity::Streaming,
            Activity::Executing,
            Activity::Complete,
            Activity::Failed,
        ] {
            let mut state = state();
            state.activity = activity;
            state.animation_frame = 2;
            state.input = if activity == Activity::Complete {
                "/mo"
            } else {
                "explain the request failure"
            }
            .into();
            let notebook = Notebook {
                cells: vec![CellView {
                    table: Some(
                        "config  File  gateway.toml · 42 lines\n  model = requested_model".into(),
                    ),
                    error: if activity == Activity::Failed {
                        Some(CellError {
                            class: "RequestError".into(),
                            message: "request failed: model unavailable".into(),
                            line: Some(3),
                            column: Some(1),
                        })
                    } else {
                        None
                    },
                    returned: if activity == Activity::Complete {
                        Some("The request uses an unavailable model.".into())
                    } else {
                        None
                    },
                    ..CellView::default()
                }],
                ..Notebook::default()
            };
            let conversation = if activity == Activity::Starting {
                Conversation {
                    system: String::new(),
                    messages: vec![],
                }
            } else {
                conversation()
            };
            let buffer = draw(width, 30, &state, &conversation, &notebook);
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{width}-{}.txt", activity.label())),
                text(&buffer),
            )
            .unwrap();
        }
    }
}

#[test]
fn natural_narration_and_explicit_output_survive_while_long_details_fold_locally() {
    let source = (0..24)
        .map(|i| format!("const item_{i} = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "inspect the file"),
            Message::text(
                Role::Assistant,
                format!("I will inspect it.\n\n```sterna\n{source}\n```\nThe result follows."),
            ),
            Message::text(Role::Assistant, "USER ANSWER"),
        ],
    };
    let notebook = Notebook {
        cells: vec![CellView {
            table: Some(
                (0..20)
                    .map(|i| format!("preview_{i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            stdout: Some("EXPLICIT OUTPUT".into()),
            returned: Some("USER ANSWER".into()),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    let mut state = state();
    state.compact = true;
    let folded = text(&draw(120, 90, &state, &conversation, &notebook));
    for text in [
        "I will inspect it.",
        "The result follows.",
        "EXPLICIT OUTPUT",
        "USER ANSWER",
        "Ctrl-O · code and results",
    ] {
        assert!(folded.contains(text));
    }
    assert_eq!(folded.matches("USER ANSWER").count(), 1);
    assert!(!folded.contains("const item_23"));
    assert!(!folded.contains("preview_19"));
    state.compact = false;
    let expanded = text(&draw(120, 90, &state, &conversation, &notebook));
    assert!(expanded.contains("const item_23 = 23;"));
    assert!(expanded.contains("preview_19"));
}

#[test]
fn transparent_themes_keep_blocks_separate_and_preserve_text() {
    use ratatui::style::Color;
    use sterna::tui::Theme;
    let mut state = state();
    state.theme = Theme::Amber;
    let amber = draw(120, 40, &state, &conversation(), &Notebook::default());
    assert!(amber.content.iter().any(|cell| cell.bg == Color::Reset));
    // Transparent everywhere the transcript is not deliberately tinted: the
    // answer block carries the theme's hush and nothing else fills a row.
    let regions = screen_regions(amber.area, &state);
    let hush = amber[(regions.transcript.x, answer_row(&amber, &regions))].bg;
    assert_ne!(hush, Color::Reset);
    for y in regions.transcript.y..regions.transcript.bottom() {
        for x in regions.transcript.x..regions.transcript.right() {
            let bg = amber[(x, y)].bg;
            assert!(
                bg == Color::Reset || bg == hush,
                "({x},{y}) is neither transparent nor the answer's hush: {bg:?}"
            );
        }
    }
    assert_ne!(
        amber[(regions.input.x, regions.input.y + 1)].bg,
        Color::Reset
    );
    assert!(
        amber
            .content
            .iter()
            .any(|cell| cell.fg == Color::LightYellow)
    );
    let rendered = text(&amber);
    assert!(rendered.contains("USER"));
    assert!(!rendered.contains("╭─") && !rendered.contains("╰─"));
    assert!(
        (regions.transcript.y..regions.transcript.bottom()).any(|y| {
            (regions.transcript.x..regions.transcript.right())
                .all(|x| amber[(x, y)].symbol().trim().is_empty())
        })
    );
    state.theme = Theme::Ice;
    let ice = draw(120, 40, &state, &conversation(), &Notebook::default());
    assert_eq!(text(&ice), rendered);
    assert!(ice.content.iter().any(|cell| cell.fg == Color::LightCyan));
}

#[test]
fn partial_responses_stay_in_the_active_block_and_only_the_indicator_moves() {
    let mut state = state();
    state.activity = Activity::Streaming;
    state.streaming_text = Some("A partial response with an unfinished ```sterna fence".into());
    let first = draw(80, 40, &state, &conversation(), &Notebook::default());
    state.animation_frame += 1;
    let next = draw(80, 40, &state, &conversation(), &Notebook::default());
    assert!(text(&first).contains("STERNA / RECEIVING"));
    assert!(text(&first).contains("unfinished ```sterna fence"));
    let region = screen_regions(first.area, &state).transcript;
    assert_ne!(first, next);
    for y in region.y..region.bottom() {
        let row: String = (region.x..region.right())
            .map(|x| first[(x, y)].symbol())
            .collect();
        if !row.contains("STERNA / RECEIVING") {
            for x in region.x..region.right() {
                assert_eq!(first[(x, y)], next[(x, y)]);
            }
        }
    }
    state.streaming_text = None;
    state.activity = Activity::Complete;
    let done = draw(80, 40, &state, &conversation(), &Notebook::default());
    assert!(!text(&done).contains("STERNA / RECEIVING"));
}

#[test]
fn composer_grows_with_text_and_statusline_preferences_preserve_bounds() {
    use sterna::tui::StatusLine;
    let mut state = state();
    let small = screen_regions(ratatui::layout::Rect::new(0, 0, 80, 40), &state);
    state.input = "line\n".repeat(8);
    let large = screen_regions(ratatui::layout::Rect::new(0, 0, 80, 40), &state);
    assert!(large.input.height > small.input.height);
    assert!(large.transcript.bottom() <= large.input.y);
    for (setting, height) in [(StatusLine::Compact, 1), (StatusLine::Hidden, 0)] {
        state.status_line = setting;
        let regions = screen_regions(ratatui::layout::Rect::new(0, 0, 80, 40), &state);
        assert_eq!(regions.status.height, height);
        assert_eq!(regions.input.bottom(), regions.status.y);
    }
}

#[test]
fn code_is_formatted_locally_and_original_source_is_available_expanded() {
    let source = "const result={ok:true,items:[1,2]};if(result.ok){console.log(result);}";
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Inspect."),
            Message::text(Role::Assistant, format!("```sterna\n{source}\n```")),
        ],
    };
    let mut state = state();
    state.pretty = true;
    let pretty = text(&draw(200, 40, &state, &conversation, &Notebook::default()));
    assert!(pretty.contains("const result ="), "{pretty}");
    assert!(pretty.contains("  console.log(result);"), "{pretty}");
    state.pretty = false;
    let raw = text(&draw(200, 40, &state, &conversation, &Notebook::default()));
    assert!(raw.contains(source));
    let sterna::contract::Block::Text(stored) = &conversation.messages[1].content[0] else {
        panic!("fixture must remain plain text")
    };
    assert!(stored.contains(source));
}

#[test]
fn transparent_sections_use_the_available_transcript_width() {
    for width in [60, 80, 120, 200, 300] {
        for sidebar in [SidebarVisibility::Auto, SidebarVisibility::Hidden] {
            let mut state = state();
            state.sidebar = sidebar;
            let buffer = draw(width, 30, &state, &conversation(), &Notebook::default());
            let regions = screen_regions(buffer.area, &state);
            let expected = if regions.details.width > 0 {
                width - 36
            } else {
                width
            };
            assert_eq!(regions.transcript.width, expected);
            let y = regions.transcript.y;
            assert_eq!(
                buffer[(regions.transcript.right() - 1, y)].bg,
                ratatui::style::Color::Reset
            );
        }
    }
}

#[test]
fn compact_view_hides_protocol_noise_and_keeps_actual_results() {
    let mut state = state();
    state.compact = true;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Read roman.py"),
            Message::text(
                Role::Assistant,
                "```sterna-edit\n{}\n```\n```sterna\nreturn 'guess';\n```",
            ),
        ],
    };
    let shown = text(&draw(120, 35, &state, &c, &Notebook::default()));
    assert!(shown.contains("Response format rejected"));
    assert!(shown.contains("nothing ran"));
    assert!(!shown.contains("```"));
    assert!(!shown.contains("return 'guess'"));
    assert!(!shown.contains("TOOL / PREVIEW"));
}

#[test]
fn speculative_calls_are_never_presented_as_observed_results() {
    let mut state = state();
    state.compact = true;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Check the branch."),
            Message::text(
                Role::Assistant,
                "```sterna\nif (false) { await bash({command: 'echo skipped'}); } return 'done';\n```",
            ),
        ],
    };
    let n = Notebook {
        cells: vec![CellView {
            execution: Some("No tool calls ran in this cell.".into()),
            returned: Some("done".into()),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    let shown = text(&draw(120, 35, &state, &c, &n));
    assert!(shown.contains("planned: bash"));
    assert!(shown.contains("No tools ran."));
    assert!(!shown.contains("bash · returned"));
    assert!(!shown.contains("inference turn"));
}

#[test]
fn completion_scan_is_bounded_and_does_not_change_history() {
    let mut state = state();
    state.activity = Activity::Complete;
    state.completion_tick = Some(0);
    let first = draw(120, 35, &state, &conversation(), &Notebook::default());
    state.completion_tick = Some(4);
    let next = draw(120, 35, &state, &conversation(), &Notebook::default());
    assert_ne!(first, next);
    let region = screen_regions(first.area, &state).transcript;
    for y in region.y..region.bottom() {
        for x in region.x..region.right() {
            assert_eq!(first[(x, y)], next[(x, y)]);
        }
    }
    state.completion_tick = None;
    let settled = text(&draw(
        120,
        35,
        &state,
        &conversation(),
        &Notebook::default(),
    ));
    assert!(settled.contains("complete"));
    assert!(!settled.contains("cell completed"));
}

#[test]
fn local_file_diffs_are_visible_in_compact_and_expanded_views() {
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Edit the file"),
            Message::text(Role::Assistant, "```sterna\nreturn 'done';\n```"),
        ],
    };
    let n = Notebook {
        cells: vec![CellView {
            changes: Some(
                "--- example.py\n+++ example.py\n@@ -1,1 +1,1 @@\n-old_value\n+new_value".into(),
            ),
            execution: Some("No tool calls ran in this cell.".into()),
            returned: Some("done".into()),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    for compact in [true, false] {
        let mut state = state();
        state.compact = compact;
        let shown = text(&draw(120, 40, &state, &c, &n));
        assert!(shown.contains("CHANGES OBSERVED"), "{shown}");
        assert!(shown.contains("-old_value"), "{shown}");
        assert!(shown.contains("+new_value"), "{shown}");
    }
}

#[test]
fn natural_answers_render_tables_and_never_invent_tool_sections() {
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Report your findings."),
            Message::text(
                Role::Assistant,
                "## Findings\n**Verified:** reads work.\n\n| Feature | Result |\n|---|---|\n| Read | Correct |\n| Write | Nested files work |",
            ),
        ],
    };
    for width in [60, 80, 120, 200] {
        for compact in [true, false] {
            let mut state = state();
            state.compact = compact;
            let shown = text(&draw(
                width,
                30,
                &state,
                &conversation,
                &Notebook::default(),
            ));
            assert!(shown.contains("Findings"));
            assert!(shown.contains("Nested files work"));
            assert!(!shown.contains("## Findings"));
            assert!(!shown.contains("**Verified:**"));
            assert!(!shown.contains("TOOL / PREVIEW"));
            assert!(!shown.contains("(no outputs)"));
        }
    }
}

#[test]
fn telemetry_distinguishes_proposals_measurements_and_actual_execution() {
    use sterna::telemetry::RequestMeasurement;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Inspect one file"),
            Message::text(
                Role::Assistant,
                "```sterna\nawait read({path:'roman.py'}); if(false) await glob({pattern:'*'});\n```",
            ),
        ],
    };
    let n = Notebook {
        requests: vec![RequestMeasurement::from_response(
            1,
            "deepseek-v4-flash".into(),
            1200,
            ServedBy::default(),
            Some(&sterna::wire::Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: Some(80),
                cache_creation_input_tokens: Some(15),
            }),
        )],
        cells: vec![CellView {
            execution: Some("└─ read roman.py · returned".into()),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    for width in [60, 80, 120, 200] {
        let mut state = state();
        state.telemetry_open = true;
        let buffer = draw(width, 44, &state, &c, &n);
        let shown = text(&buffer);
        for expected in [
            "TELEMETRY",
            "REQUEST 01",
            "read roman.py",
            "cost unreported",
            "Cached input 80 · cache write 15",
            "proposed",
            "observed",
        ] {
            assert!(shown.contains(expected), "{expected} at {width}: {shown}");
        }
        assert!(!shown.contains("glob · returned"));
        assert!(!shown.contains("tokens saved"));
        assert!(!shown.contains("$0"));
        let regions = screen_regions(buffer.area, &state);
        assert!(regions.transcript.bottom() <= regions.input.y);
    }
}

#[test]
fn telemetry_motion_is_local_and_reduced_motion_stays_still() {
    let c = conversation();
    let mut state = state();
    state.telemetry_open = true;
    state.activity = Activity::Thinking;
    let first = draw(120, 44, &state, &c, &Notebook::default());
    state.animation_frame = 14;
    let moved = draw(120, 44, &state, &c, &Notebook::default());
    assert_ne!(first, moved);
    state.reduced_motion = true;
    let reduced = draw(120, 44, &state, &c, &Notebook::default());
    state.animation_frame = 24;
    let later = draw(120, 44, &state, &c, &Notebook::default());
    // The telemetry body is still. The legacy global activity indicator has
    // its own clock, frozen by the live loop when reduced motion is selected.
    let area = screen_regions(reduced.area, &state).transcript;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            assert_eq!(reduced[(x, y)], later[(x, y)]);
        }
    }
}

#[test]
fn delivery_trace_counts_real_bytes_and_has_bounded_history() {
    let mut pulse = sterna::tui::Pulse::default();
    for _ in 0..50 {
        pulse.receive(7);
    }
    assert_eq!(pulse.bytes, 350);
    assert_eq!(pulse.deltas, 50);
    assert_eq!(pulse.deliveries.len(), 32);
}

#[test]
fn cell_repair_shows_the_amended_code_and_keeps_the_original_failure() {
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Answer briefly"),
            Message::text(Role::Assistant, "```sterna\nreturn 'ok;\n```"),
            Message::text(Role::User, "SyntaxError; repair is available"),
            Message::text(
                Role::Assistant,
                "```sterna-edit\n{\"cell\":1,\"replace\":\"'ok;\",\"with\":\"'ok';\"}\n```",
            ),
        ],
    };
    let n = Notebook {
        cells: vec![
            CellView {
                error: Some(CellError {
                    class: "SyntaxError".into(),
                    message: "Unterminated string".into(),
                    line: Some(1),
                    column: Some(7),
                }),
                answered: true,
                execution: Some("No tool calls ran in this cell.".into()),
                ..CellView::default()
            },
            CellView {
                executed_source: Some("return 'ok';".into()),
                repaired_from: Some(1),
                returned: Some("ok".into()),
                execution: Some("No tool calls ran in this cell.".into()),
                ..CellView::default()
            },
        ],
        ..Notebook::default()
    };
    for compact in [true, false] {
        let mut state = state();
        state.compact = compact;
        let shown = text(&draw(100, 44, &state, &c, &n));
        assert!(shown.contains("Unterminated string"), "{shown}");
        assert!(shown.contains("Amends syntax-failed cell 1"), "{shown}");
        assert!(!shown.contains("\"replace\""), "{shown}");
        if compact {
            // The state is the header field's own word now, not a sentence
            // beside a corner glyph.
            assert!(shown.contains("REPAIRED"), "{shown}");
        } else {
            assert!(shown.contains("return 'ok';"));
        }
    }
}

#[test]
fn activity_ribbons_use_reserved_space_and_reduced_motion_keeps_them_still() {
    let c = conversation();
    let n = Notebook::default();
    for width in [60, 80, 120, 200] {
        let mut state = state();
        state.activity = Activity::Streaming;
        let first = draw(width, 40, &state, &c, &n);
        let regions = screen_regions(first.area, &state);
        assert!(regions.activity.height > 0);
        assert!(regions.transcript.bottom() <= regions.activity.y);
        assert!(regions.activity.bottom() <= regions.input.y);
        state.animation_frame = 15;
        let moved = draw(width, 40, &state, &c, &n);
        assert_ne!(first, moved);
        state.reduced_motion = true;
        let reduced = draw(width, 40, &state, &c, &n);
        state.animation_frame = 30;
        let later = draw(width, 40, &state, &c, &n);
        for y in regions.activity.y..regions.activity.bottom() {
            for x in regions.activity.x..regions.activity.right() {
                assert_eq!(reduced[(x, y)], later[(x, y)]);
            }
        }
        assert_eq!(
            screen_regions(Rect::new(0, 0, width, 20), &state)
                .activity
                .height,
            0
        );
        state.activity = Activity::Idle;
        assert_eq!(screen_regions(first.area, &state).activity.height, 0);
    }
}

#[test]
fn ordered_action_blocks_do_not_leak_a_second_raw_code_block_into_narration() {
    let mut live = state();
    live.compact = true;
    let task = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Read two files"),
            Message::text(
                Role::Assistant,
                "BEFORE_PROSE\n```sterna\nconst a = await read({path: 'a'});\n```\nBETWEEN_PROSE\n```sterna\nconst hidden_second_source = await read({path: 'b'});\n```\nAFTER_PROSE",
            ),
        ],
    };
    let notebook = Notebook {
        cells: vec![CellView {
            execution: Some("read a · returned\nread b · returned".into()),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    let shown = text(&draw(120, 42, &live, &task, &notebook));
    for prose in ["BEFORE_PROSE", "BETWEEN_PROSE", "AFTER_PROSE"] {
        assert_eq!(shown.matches(prose).count(), 1, "{shown}");
    }
    assert!(
        !shown.contains("hidden_second_source"),
        "second source leaked out of collapsed actions: {shown}"
    );
}

// --- docs/helpers.md, "In the TUI" ------------------

fn helper_record(gave: &str, ok: bool, elapsed_ms: u64) -> HelperRecord {
    HelperRecord {
        helper: "reduce".to_string(),
        verb: "reducing".to_string(),
        asked: "cargo build log, 4118 lines".to_string(),
        outcome: HelperOutcome {
            text: gave.to_string(),
            ok,
            cancelled: false,
            elapsed_ms,
        },
        turns: 1,
        looked: Vec::new(),
        usage: Default::default(),
    }
}

fn helper_notebook(records: Vec<HelperRecord>) -> Notebook {
    Notebook {
        cells: vec![CellView {
            helpers: records,
            executed_source: Some("const n = 1;".to_string()),
            execution: Some("No tool calls ran in this cell.".to_string()),
            ..CellView::default()
        }],
        ..Notebook::default()
    }
}

/// *Is it alive* is the question the lane answers, so `/motion off` freezes
/// the glyph and leaves the seconds counting.
#[test]
fn reduced_motion_freezes_the_helper_glyph_and_keeps_its_seconds() {
    let notebook = helper_notebook(vec![helper_record("", false, 2000)]);
    let lane = |state: &ScreenState| {
        let rendered = text(&draw(110, 30, state, &conversation(), &notebook));
        rendered
            .lines()
            .find(|line| line.contains("reducing"))
            .unwrap_or_else(|| panic!("the lane renders:\n{rendered}"))
            .to_string()
    };

    let mut state = state();
    state.reduced_motion = true;
    let still = lane(&state);
    state.animation_frame = 2;
    assert_eq!(still, lane(&state), "reduced motion must freeze the glyph");
    assert!(
        still.contains("2.0s"),
        "the seconds are text, not animation: {still}"
    );

    state.reduced_motion = false;
    assert_ne!(
        still,
        lane(&state),
        "with motion on the glyph must move: {still}"
    );
}

/// A long transcript, so the transcript region has more rows than it can show.
fn long_conversation() -> Conversation {
    let mut messages = Vec::new();
    for turn in 0..40 {
        messages.push(Message::text(Role::User, format!("question {turn}")));
        messages.push(Message::text(Role::Assistant, format!("answer {turn}")));
    }
    Conversation {
        system: String::new(),
        messages,
    }
}

/// The column the scroll overlay may use: the transcript's own last column.
fn indicator_column(width: u16, height: u16, state: &ScreenState) -> u16 {
    screen_regions(Rect::new(0, 0, width, height), state)
        .transcript
        .right()
        - 1
}

fn column_symbols(buffer: &Buffer, column: u16) -> String {
    (buffer.area.y..buffer.area.bottom())
        .map(|y| buffer[(column, y)].symbol())
        .collect()
}

#[test]
fn nothing_marks_the_scroll_at_the_live_edge() {
    let state = state();
    let buffer = draw(80, 30, &state, &long_conversation(), &Notebook::default());
    let column = column_symbols(&buffer, indicator_column(80, 30, &state));
    assert!(
        !column.contains('↓') && !column.contains('▐'),
        "the resting screen draws no scroll furniture, got {column:?}"
    );
}

#[test]
fn a_scrolled_transcript_says_there_is_more_below() {
    let state = ScreenState {
        scrollback: 12,
        ..state()
    };
    let buffer = draw(80, 30, &state, &long_conversation(), &Notebook::default());
    let column = column_symbols(&buffer, indicator_column(80, 30, &state));
    assert!(
        column.contains('↓'),
        "scrolled away from the live edge, the hidden content below is marked, got {column:?}"
    );
    assert!(
        !column.contains('▐'),
        "the position indicator belongs to an active scroll only, got {column:?}"
    );
}

#[test]
fn the_scroll_indicator_is_one_column_over_the_transcript_never_beside_the_sidebar() {
    let state = ScreenState {
        scrollback: 12,
        scrolling: true,
        sidebar: SidebarVisibility::Shown,
        ..state()
    };
    // Wide enough that the sidebar is drawn beside the transcript.
    let regions = screen_regions(Rect::new(0, 0, 160, 30), &state);
    assert!(regions.details.width > 0, "this case needs the sidebar");
    let buffer = draw(160, 30, &state, &long_conversation(), &Notebook::default());
    let column = indicator_column(160, 30, &state);
    assert!(
        column < regions.details.x,
        "the indicator's column {column} must lie inside the transcript, \
         left of the sidebar at {}",
        regions.details.x
    );
    assert!(
        column_symbols(&buffer, column).contains('▐'),
        "an active scroll shows the position indicator"
    );
    for x in regions.details.x..regions.details.right() {
        let drawn = column_symbols(&buffer, x);
        assert!(
            !drawn.contains('▐') && !drawn.contains('↓'),
            "no scroll mark may be drawn in the sidebar's columns, column {x} has {drawn:?}"
        );
    }
}

#[test]
fn only_a_released_mouse_is_marked_on_the_status_line() {
    let captured = draw(120, 30, &state(), &conversation(), &Notebook::default());
    assert!(
        !text(&captured).contains("mouse off"),
        "captured is the default and needs no permanent marker"
    );
    assert!(
        text(&captured).contains("/mouse"),
        "but the idle hint still says how to free the pointer for selection"
    );
    let released = ScreenState {
        mouse_off: true,
        ..state()
    };
    let buffer = draw(120, 30, &released, &conversation(), &Notebook::default());
    assert!(
        text(&buffer).contains("mouse off"),
        "a released pointer must be visible, or dead clicks read as a broken TUI"
    );
}

#[test]
fn the_context_reading_outranks_the_mouse_marker_on_a_narrow_status_line() {
    let released = ScreenState {
        mouse_off: true,
        ..state()
    };
    let notebook = Notebook {
        context: Some(ContextTokens {
            used: 123_000,
            cap: Some(200_000),
            cap_source: sterna::models::WindowSource::Observed,

            counted: Counted::Gateway,
        }),
        ..Notebook::default()
    };
    let narrow = draw(80, 30, &released, &conversation(), &notebook);
    let rendered = text(&narrow);
    assert!(
        rendered.contains("ctx "),
        "the context reading owns the right edge at every width:\n{rendered}"
    );
    assert!(
        !rendered.contains("mouse off"),
        "and the marker stands down rather than crowding it out:\n{rendered}"
    );
}

/// The user, 2026-09-17: *"Cells are pretty tough to read."* The compact
/// screen is the default one, and before this it drew the program's own first
/// line and nothing a person had written. Now it draws the model's sentence
/// under the cell header, and the sentence the model wrote when it yielded —
/// which existed on 40 of the 123 views of the dogfooding corpus and was drawn
/// on none of them, because the compact path returned before reaching it.
#[test]
fn the_compact_cell_shows_what_it_was_for_and_why_it_stopped() {
    let mut state = state();
    state.compact = true;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "implement ssh.run"),
            Message::text(
                Role::Assistant,
                "```sterna\nconst design = await read({path: \"d.md\"});\n```",
            ),
        ],
    };
    let n = Notebook {
        cells: vec![CellView {
            description: Some("Reading the ssh design to find what I have to change.".into()),
            execution: Some("└─ read d.md · returned".into()),
            yield_reason: Some("waiting for the design before I touch the config".into()),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    let shown = text(&draw(120, 35, &state, &c, &n));
    // The descriptor is the cell's headline now, and a headline is set in
    // uppercase (`tui/poster.rs`): the words are the model's, the case is the
    // screen's.
    assert!(
        shown.contains("READING THE SSH DESIGN TO FIND WHAT I HAVE TO CHANGE."),
        "the descriptor is not on the default screen:\n{shown}"
    );
    assert!(
        shown.contains("yielded: waiting for the design before I touch the config"),
        "the yield reason is still hidden in compact:\n{shown}"
    );
}

/// Absent, nothing is drawn in its place: the change degrades to exactly the
/// screen this cell had before the descriptor existed.
#[test]
fn a_cell_whose_model_said_nothing_draws_no_empty_descriptor_row() {
    let mut state = state();
    state.compact = true;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "implement ssh.run"),
            Message::text(Role::Assistant, "```sterna\nreturn 1;\n```"),
        ],
    };
    let described = |description: Option<String>| {
        let n = Notebook {
            cells: vec![CellView {
                description,
                execution: Some("└─ read d.md · returned".into()),
                ..CellView::default()
            }],
            ..Notebook::default()
        };
        text(&draw(120, 35, &state, &c, &n))
    };
    assert_eq!(
        described(None),
        described(Some(String::new())),
        "an empty descriptor drew a row the absent one did not"
    );
}

/// **A rung and a grant are two choices; whether Sterna confines what it
/// spawns is the third, and until 2026-09-19 no surface said it.** A person
/// who had set `permissions.mode = full` and a `Bash` grant read `execute ·
/// full` as confirmation, and a session spent twelve cells hunting a linker
/// the seatbelt was never going to let it run. The posture row now carries
/// the word wherever it fits.
#[test]
fn the_posture_row_says_whether_this_session_is_confined() {
    let mut confined = state();
    confined.confinement = Some("confined".into());
    let rendered = text(&draw(
        120,
        24,
        &confined,
        &conversation(),
        &Notebook::default(),
    ));
    assert!(
        rendered.contains("sandbox 3p/4c confined"),
        "the confined state is named, not left to be assumed:\n{rendered}"
    );

    let mut open = state();
    open.sandbox = Some("3p/1c YOLO".into());
    open.confinement = Some("unconfined".into());
    let rendered = text(&draw(120, 24, &open, &conversation(), &Notebook::default()));
    assert!(
        rendered.contains("sandbox 3p/1c YOLO unconfined"),
        "and so is the unconfined one, beside the admission half:\n{rendered}"
    );

    // The one thing that outranks it. `tui_live`'s resize test pins the
    // context reading as this row's right-edge signal, and at 60 columns a
    // longer left half takes it away -- so the word is dropped there rather
    // than the reading, and the startup line and `sterna doctor` carry it.
    let narrow = text(&draw(60, 24, &open, &conversation(), &Notebook::default()));
    assert!(
        !narrow.contains("unconfined"),
        "a narrow row keeps the context reading instead:\n{narrow}"
    );
}

/// The fixture the poster tests read: one executed cell with an intent, a
/// handle table of four bindings and six host calls.
fn poster_fixture() -> (Conversation, Notebook) {
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Separate the two halves of full access."),
            Message::text(
                Role::Assistant,
                "```sterna\nconst hits = await rg({pattern: 'container_mode'});\n```",
            ),
        ],
    };
    let notebook = Notebook {
        cells: vec![CellView {
            description: Some(
                "I'm extracting the remaining coupled expressions, before applying the split"
                    .into(),
            ),
            table: Some(
                "profileCoupling   Grep.Match[]   n=18   inline cost ~1,178 tok · preview 129 tok\n  [0] \"profile.rs:145\"\nstartupCoupling   Grep.Match[]   n=19   inline cost ~900 tok · preview 90 tok\nmanifestCoupling  Grep.Match[]   n=5   inline cost ~200 tok · preview 40 tok\ndocsCoupling      Grep.Match[]   n=5   inline cost ~210 tok · preview 44 tok\n"
                    .into(),
            ),
            execution: Some(
                "├─ rg profile.rs · returned\n├─ rg manifest.rs · returned\n├─ context grants.md · returned\n└─ rg registry.rs · returned"
                    .into(),
            ),
            call_count: Some(4),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    (conversation, notebook)
}

/// **The defect the user actually pointed at.** The readable form of a cell's
/// bindings already existed and was given to the model and not to the person:
/// the column drew the cell's raw stdout, which for a real cell was four
/// kilobytes of one-line JSON. Rows, with the name, the type and the length —
/// and never a brace.
#[test]
fn a_finished_cell_is_identical_across_frames_while_a_live_one_moves() {
    // The user, watching a live run: *"vor allem dass alle blinken"*. Every
    // cell used to be handed the same `animation_frame`, so a screen of
    // finished records cycled the reveal ramp in lockstep. This asserts the
    // wiring, not the contract: `poster` already renders a still field for a
    // tick of zero, and what was wrong was which cells got one.
    let (c, n) = poster_fixture();
    let mut early = state();
    early.compact = true;
    early.animation_frame = 9;
    let mut later = state();
    later.compact = true;
    later.animation_frame = 10;
    let a = text(&draw(120, 34, &early, &c, &n));
    let b = text(&draw(120, 34, &later, &c, &n));
    let header = |screen: &str| {
        screen
            .lines()
            .find(|line| line.contains("/ CELL"))
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        header(&a),
        header(&b),
        "a finished cell moved between frames:\n{a}"
    );

    // And the guard against over-fixing: a cell still running must move, or
    // the screen reads as stalled.
    let mut running = n.clone();
    running.cells[0].execution = None;
    let live_a = text(&draw(120, 34, &early, &c, &running));
    let live_b = text(&draw(120, 34, &later, &c, &running));
    assert_ne!(
        header(&live_a),
        header(&live_b),
        "the running cell stopped moving:\n{live_a}"
    );
}

#[test]
fn a_cells_bindings_are_rows_and_never_json() {
    let (c, n) = poster_fixture();
    let mut state = state();
    state.compact = true;
    let shown = text(&draw(120, 34, &state, &c, &n));
    for name in [
        "profileCoupling",
        "startupCoupling",
        "manifestCoupling",
        "docsCoupling",
    ] {
        assert!(shown.contains(name), "binding {name} is missing:\n{shown}");
    }
    assert!(
        shown.contains("Grep.Match[]"),
        "the type is missing:\n{shown}"
    );
    assert!(
        shown.contains("   01  ") && shown.contains("   02  "),
        "the rows are not numbered:\n{shown}"
    );
    // The token costs are the model's budget, not the reader's.
    assert!(
        !shown.contains("inline cost"),
        "a row carried the model's budget:\n{shown}"
    );
    let cell_block: String = shown
        .lines()
        .skip_while(|line| !line.contains("/ CELL"))
        .take_while(|line| !line.contains("OUTPUT"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !cell_block.contains('{') && !cell_block.contains("\": "),
        "the cell block still renders JSON:\n{cell_block}"
    );
}

/// The intent is the headline, and the header is a filled field — the two
/// things the user asked for first.
#[test]
fn the_intent_is_the_headline_above_a_filled_field() {
    let (c, n) = poster_fixture();
    let mut state = state();
    state.compact = true;
    let shown = text(&draw(120, 34, &state, &c, &n));
    assert!(
        shown.contains("EXTRACTING THE REMAINING COUPLED EXPRESSIONS"),
        "the intent is not the headline:\n{shown}"
    );
    assert!(
        shown.contains("▸ before applying the split"),
        "the qualifier is missing:\n{shown}"
    );
    assert!(
        shown.contains("█ 01 / CELL") && shown.contains("EXECUTED"),
        "the header is not a filled field:\n{shown}"
    );
    // Mixed kinds are named rather than totalled away.
    assert!(
        shown.contains("rg ×3 · context"),
        "the call bar lost its kinds:\n{shown}"
    );
}

/// `/motion off` loses decoration and nothing else: the same screen, still.
#[test]
fn a_motion_off_cell_renders_complete_and_identical() {
    let (c, n) = poster_fixture();
    let mut moving = state();
    moving.compact = true;
    moving.animation_frame = 3;
    let mut still = moving.clone();
    still.reduced_motion = true;
    let stilled = text(&draw(120, 34, &still, &c, &n));
    for fragment in [
        "01 / CELL",
        "EXECUTED",
        "EXTRACTING THE REMAINING",
        "profileCoupling",
        "   04  ",
    ] {
        assert!(
            stilled.contains(fragment),
            "motion off lost {fragment}:\n{stilled}"
        );
    }
    assert!(
        !stilled.contains('░') && !stilled.contains('▒'),
        "a still frame used a reveal glyph:\n{stilled}"
    );
}

/// A filled field has a ground as well as an ink, so every theme has to be
/// checked rather than only the default.
#[test]
fn the_header_field_draws_in_every_theme() {
    use sterna::tui::Theme;
    let (c, n) = poster_fixture();
    for theme in Theme::ALL {
        let mut state = state();
        state.compact = true;
        state.theme = theme;
        let shown = text(&draw(120, 34, &state, &c, &n));
        assert!(
            shown.contains("01 / CELL") && shown.contains("EXECUTED"),
            "{theme:?} lost the header labels:\n{shown}"
        );
        assert!(
            shown.contains('█'),
            "{theme:?} lost the field itself:\n{shown}"
        );
    }
}

#[test]
fn the_spend_rail_drops_the_qualifiers_that_fire_every_session() {
    use sterna::tui::{HelperModelTokens, HelperTokens, TaskTokens};
    let mut n = Notebook::default();
    n.tokens = Some(TaskTokens {
        used: 1_100_000,
        parent_used: 766_000,
        helpers: HelperTokens {
            calls: 3,
            usage_known_calls: 3,
            used: 285_900,
            requests: 9,
            reported_requests: 9,
            cache_read_reported_requests: 9,
            cache_creation_reported_requests: 9,
            models: vec![HelperModelTokens {
                model: "gpt-5.6-luna".into(),
                calls: 3,
                usage_known_calls: 3,
                used: 285_900,
                requests: 9,
                reported_requests: 9,
                cache_read_reported_requests: 9,
                cache_creation_reported_requests: 9,
                ..HelperModelTokens::default()
            }],
            ..HelperTokens::default()
        },
        counted: Counted::Gateway,
    });
    let shown = text(&draw(200, 44, &state(), &conversation(), &n));
    for noise in [
        "responses 9/9",
        "coverage partial",
        "helper coverage partial",
        "cumulative task spend",
        "counted: reported",
        "cache create unreported",
        "inbox 0",
        "batches 0",
        "handlers 0",
    ] {
        assert!(!shown.contains(noise), "{noise:?} survived:\n{shown}");
    }
    assert!(shown.contains("parent"), "the split must survive:\n{shown}");
    assert!(
        shown.contains("gpt-5.6-luna"),
        "one helper model folds onto the split rather than vanishing:\n{shown}"
    );
}

/// A partial count marks the number rather than printing a line beside it, so
/// the headline never looks exact when it is a known-low subtotal.
#[test]
fn an_incomplete_helper_count_marks_the_total_rather_than_captioning_it() {
    use sterna::tui::{HelperModelTokens, HelperTokens, TaskTokens};
    let mut n = Notebook::default();
    n.tokens = Some(TaskTokens {
        used: 900_000,
        parent_used: 800_000,
        helpers: HelperTokens {
            calls: 4,
            usage_known_calls: 2,
            used: 100_000,
            requests: 4,
            reported_requests: 2,
            models: vec![HelperModelTokens {
                model: "gpt-5.6-luna".into(),
                calls: 4,
                usage_known_calls: 2,
                used: 100_000,
                requests: 4,
                reported_requests: 2,
                ..HelperModelTokens::default()
            }],
            ..HelperTokens::default()
        },
        counted: Counted::Estimated,
    });
    let shown = text(&draw(200, 44, &state(), &conversation(), &n));
    assert!(
        shown.contains('+'),
        "an at-least figure must say so on the number:\n{shown}"
    );
    assert!(
        shown.contains("estimated"),
        "provenance that is not the gateway's row must show:\n{shown}"
    );
}
