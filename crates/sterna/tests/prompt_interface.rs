//! The interface-aware system block — `model-contract.md` §2.1 and the
//! `## Environment` manifest block (`smarter-cheaper-roadmap.md`, *Hybrid
//! interface choice* and *Capability/environment manifest*).
//!
//! The measured defect: in hybrid mode the shipped preamble said
//! `execute_cell` was the only provider-native tool while the same request
//! declared the direct tools, and the pilot's model chose `execute_cell` for
//! all 91 acting turns. These tests pin that each variant describes the
//! routes the request actually declares.

use sterna::abi::Interface;
use sterna::manifest::Manifest;
use sterna::prompt::{self, SessionFacts, declarations};
use sterna::tools::registry;

fn facts(interface: Interface, manifest: Option<String>) -> SessionFacts {
    SessionFacts {
        root: "/tmp/x".to_string(),
        writable: vec!["Write(src/**)".to_string()],
        network: false,
        interface,
        manifest,
    }
}

fn system(interface: Interface, manifest: Option<String>) -> String {
    prompt::render_system(
        "PROJECT-INSTRUCTION",
        &registry::ALL.iter().collect::<Vec<_>>(),
        &facts(interface, manifest),
    )
}

/// The sentences every variant keeps: the guidance is shared, not copied.
const SHARED: &[&str] = &[
    "You are Sterna, a coding assistant. Answer conversational questions naturally.",
    "Bindings persist between cells of this user request",
    "returning a value displays it\nas notebook output and finishes nothing",
    "The task ends only where you say it ends.",
    "only when the request is finished and the answer is grounded in",
    "PermissionDenied is\nfinal: code cannot widen the session's sandbox grant.",
];

#[test]
fn the_hybrid_preamble_names_both_routes_and_denies_neither() {
    let hybrid = prompt::preamble_for(Interface::Hybrid);
    for denial in [
        "only provider-native tool",
        "only\nprovider-native tool",
        "callable only inside its code",
        "no `execute_cell` call",
    ] {
        assert!(
            !hybrid.contains(denial),
            "hybrid preamble says {denial:?}:\n{hybrid}"
        );
    }
    assert!(
        hybrid.contains("call a familiar tool directly for one independent operation"),
        "{hybrid}"
    );
    assert!(
        hybrid.contains(
            "exactly one `execute_cell` call for dependent, branching, looped or batched"
        ),
        "{hybrid}"
    );
    assert!(
        hybrid.contains("Inside a cell the same tools are typed async functions"),
        "{hybrid}"
    );
    // The cell mechanics still apply when a cell can be sent.
    assert!(
        hybrid.contains("A cell is validated before it runs."),
        "{hybrid}"
    );
    assert!(
        hybrid.contains("While you construct the call, none of THIS cell has executed."),
        "{hybrid}"
    );
    for sentence in SHARED {
        assert!(
            hybrid.contains(sentence),
            "hybrid lost {sentence:?}:\n{hybrid}"
        );
    }
}

#[test]
fn the_tools_preamble_never_mentions_a_cell_it_cannot_send() {
    let tools = prompt::preamble_for(Interface::Tools);
    assert!(!tools.contains("execute_cell"), "{tools}");
    assert!(!tools.contains("provider-native tool"), "{tools}");
    assert!(!tools.contains("A cell is validated"), "{tools}");
    assert!(!tools.contains("template literal"), "{tools}");
    // The chaining paragraph, its worked cells and the edit rhythm are a
    // cell's economics; a request that declares no cell must not be taught
    // them, and the two host globals those examples call are bound only
    // inside one.
    for absent in [
        "A cell is a program",
        "await Promise.all",
        "helper.reduce(",
        "decide.choice(",
        "two turns for a batch of edits",
    ] {
        assert!(
            !tools.contains(absent),
            "tools preamble teaches {absent:?}:\n{tools}"
        );
    }
    assert!(
        tools.contains(
            "To act, call the familiar tools directly; each call's result is runtime\nevidence."
        ),
        "{tools}"
    );
    assert!(
        tools.contains("A prose response with no tool call ends the task as the answer."),
        "{tools}"
    );
    for sentence in SHARED {
        assert!(
            tools.contains(sentence),
            "tools lost {sentence:?}:\n{tools}"
        );
    }
}

/// A dropped segment takes its own blank line with it.
///
/// Three segments are dropped in `Tools` (the descriptor paragraph, the
/// chaining paragraph with its worked cells, and the edit-rhythm paragraph)
/// and one in `Hybrid`'s replacements. Each carries its trailing separator,
/// so a drop leaves one blank line between paragraphs rather than two.
#[test]
fn a_dropped_segment_leaves_no_hole_in_the_block() {
    for interface in [Interface::Cells, Interface::Hybrid, Interface::Tools] {
        let text = prompt::preamble_for(interface);
        assert!(
            !text.contains("\n\n\n"),
            "{interface:?} preamble has a blank-line hole:\n{text}"
        );
        assert!(
            !text.starts_with('\n'),
            "{interface:?} preamble opens blank"
        );
        assert!(
            text.ends_with("sandbox grant."),
            "{interface:?} preamble does not end on its last sentence:\n{text}"
        );
    }
}

#[test]
fn the_cells_preamble_is_the_constant() {
    assert_eq!(prompt::preamble_for(Interface::Cells), prompt::PREAMBLE);
}

#[test]
fn the_hybrid_execute_cell_description_does_not_deny_direct_calls() {
    let hybrid = declarations::execute_cell_description(Interface::Hybrid);
    assert!(!hybrid.contains("not separate native calls"), "{hybrid}");
    assert!(!hybrid.contains("exactly one native call"), "{hybrid}");
    assert!(
        hybrid.contains("The familiar tools are also callable directly"),
        "{hybrid}"
    );
    assert!(hybrid.contains("Prose and comments are not runtime evidence."));
    assert_eq!(
        declarations::execute_cell_description(Interface::Cells),
        declarations::EXECUTE_CELL_DESCRIPTION
    );
    assert!(declarations::EXECUTE_CELL_DESCRIPTION.contains("not separate native calls"));
}

#[test]
fn the_system_block_carries_the_interfaces_preamble() {
    let hybrid = system(Interface::Hybrid, None);
    assert!(
        hybrid.starts_with(&prompt::preamble_for(Interface::Hybrid)),
        "{hybrid}"
    );
    assert!(
        !hybrid.contains("callable only inside its code"),
        "{hybrid}"
    );
    let cells = system(Interface::Cells, None);
    assert!(cells.starts_with(prompt::PREAMBLE), "{cells}");
}

#[test]
fn the_whole_set_sentence_is_interface_aware() {
    let cells = prompt::render_session_facts(&facts(Interface::Cells, None));
    assert!(
        cells.contains("The tools above are the whole set. To change"),
        "{cells}"
    );

    let hybrid = prompt::render_session_facts(&facts(Interface::Hybrid, None));
    assert!(
        hybrid.contains("the familiar ones are callable directly or\ninside a cell."),
        "{hybrid}"
    );
    assert!(!hybrid.contains("whole set. To change"), "{hybrid}");

    let tools = prompt::render_session_facts(&facts(Interface::Tools, None));
    assert!(
        tools.contains("The familiar tools above are the whole set, called directly."),
        "{tools}"
    );
    // The facts after the sentence are the same in every variant.
    for text in [&cells, &hybrid, &tools] {
        assert!(text.contains("Sandbox: writable: Write(src/**)"), "{text}");
    }
}

#[test]
fn a_manifest_renders_once_as_its_own_block_after_this_session() {
    let manifest = Manifest {
        root: "/tmp/x".into(),
        readable_roots: vec!["/tmp/x".into()],
        writable_roots: vec!["/tmp/x/src".into()],
        ..Manifest::default()
    }
    .render();
    assert!(manifest.starts_with("## Environment\n\n"), "{manifest}");

    let with = system(Interface::Hybrid, Some(manifest.clone()));
    assert_eq!(
        with.matches("## Environment\n").count(),
        1,
        "the manifest block renders exactly once:\n{with}"
    );
    let session_at = with.find("## This session").unwrap();
    let manifest_at = with
        .find(&manifest)
        .expect("the rendered manifest, verbatim");
    let instructions_at = with.rfind("PROJECT-INSTRUCTION").unwrap();
    assert!(session_at < manifest_at, "{with}");
    assert!(manifest_at < instructions_at, "{with}");
    assert!(
        with.contains(&format!("\n\n{manifest}\n\nPROJECT-INSTRUCTION")),
        "the manifest is its own block between the session facts and the instructions:\n{with}"
    );

    let without = system(Interface::Hybrid, None);
    assert!(!without.contains("## Environment\n"), "{without}");
    assert_eq!(
        with.replace(&format!("\n\n{manifest}"), ""),
        without,
        "a manifest adds one block and changes nothing else"
    );
}

/// The default interface is cells-only — the user's decision of 2026-09-13
/// on the matched ablation (`smarter-cheaper-roadmap.md`, *The 2026-09-13
/// ablation*). Pinned twice: the value, and what a session started without
/// `--interface` actually shows the model — the cells constant, with no
/// direct tool declared.
#[test]
fn the_default_interface_is_cells_only() {
    assert_eq!(Interface::default(), Interface::Cells);
    assert_eq!(
        system(Interface::default(), None),
        system(Interface::Cells, None),
        "a session without --interface must show the cells-only block"
    );
    assert_eq!(
        prompt::preamble_for(Interface::default()),
        prompt::preamble_for(Interface::Cells)
    );
    assert_ne!(
        prompt::preamble_for(Interface::default()),
        prompt::preamble_for(Interface::Hybrid),
        "the default is no longer hybrid"
    );
}

/// An interface and its availability are stated in the same place.
///
/// Measured 2026-09-19: a session with `[helpers] enabled = true` and no
/// `[helpers] model` declared the whole `helper` roster with a worked
/// example, and put `Unavailable: helper.*: no helper model is configured`
/// in a separate block further down. Zero helper calls were made in 22
/// cells — the model was right, but it had to reconcile a promise and a
/// refusal that never appeared together.
#[test]
fn an_unconfigured_helper_roster_says_so_where_it_is_declared() {
    let unconfigured = declarations::helper_declaration(false);
    let configured = declarations::helper_declaration(true);

    assert_eq!(
        configured,
        declarations::HELPER_DECLARATION,
        "a configured session reads the table form unchanged"
    );
    assert!(
        unconfigured.starts_with(declarations::HELPER_DECLARATION),
        "the roster itself is unchanged; only its availability is added"
    );
    assert!(
        unconfigured.contains("no helper model is configured"),
        "{unconfigured}"
    );
    assert!(
        unconfigured.contains("`[helpers] model`"),
        "the sentence must name the key that would fix it: {unconfigured}"
    );
    assert_ne!(
        unconfigured, configured,
        "an unconfigured session must not read the same as a configured one"
    );
}

/// The session's Runtime block carries that answer, so the block being
/// reachable is not the same claim as the prompt carrying it.
#[test]
fn the_runtime_block_of_an_unhelped_session_carries_the_refusal() {
    let globals = sterna::runtime::bindings::HostGlobals::Every;
    let without = prompt::render_runtime_reaching(
        globals,
        prompt::Reach {
            helpers: Some(false),
            ..prompt::Reach::default()
        },
    );
    let with = prompt::render_runtime_reaching(
        globals,
        prompt::Reach {
            helpers: Some(true),
            ..prompt::Reach::default()
        },
    );
    assert!(
        without.contains("no helper model is configured"),
        "{without}"
    );
    assert!(!with.contains("no helper model is configured"), "{with}");
    // A caller that does not know renders the table form, so no existing
    // surface silently starts claiming helpers are missing.
    let unstated = prompt::render_runtime_reaching(globals, prompt::Reach::default());
    assert!(
        !unstated.contains("no helper model is configured"),
        "{unstated}"
    );
}
