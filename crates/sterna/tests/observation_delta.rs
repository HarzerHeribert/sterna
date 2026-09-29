//! Acceptance tests for the roadmap's *Observation delta* and *Handle
//! lifecycle* rows (`archive/glasshouse:docs/product/pane/smarter-cheaper-roadmap.md`):
//! what a turn RENDERS changes with the cell; what is LIVE never does.

use sterna::runtime::handles::{HandleMeta, HandleTable, render_table, render_table_delta};
use sterna::runtime::preview::{PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP, Value};

fn two_handles_declared_in_cell_one() -> HandleTable {
    let mut table = HandleTable::new();
    table.begin_cell(1);
    table.declare(
        "hits",
        Value::array(vec![Value::Number(1.0), Value::Number(2.0)]),
        1,
    );
    table.declare("adapter", Value::string("codex"), 1);
    table
}

/// The first rendering after a task starts is a full inventory: every
/// entry is new, so the delta is the full table byte for byte.
#[test]
fn the_first_rendering_is_the_full_inventory() {
    let table = two_handles_declared_in_cell_one();
    let full = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(delta, full);
    assert!(stats.full_inventory);
    assert_eq!(stats.rows_rendered, 2);
    assert_eq!(stats.rows_suppressed, 0);
    assert_eq!(stats.bytes_rendered, full.len());
    assert_eq!(stats.bytes_full_inventory, full.len());
    assert_eq!(stats.bytes_suppressed(), 0);
    assert_eq!(stats.repeated_observations, 0);
}

/// (a) A cell that changed nothing renders one line naming every live
/// handle, and the stats say what that saved.
#[test]
fn an_unchanged_table_is_one_line_naming_its_handles() {
    let mut table = two_handles_declared_in_cell_one();
    table.begin_cell(2);

    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(delta, "Unchanged from earlier cells (2): hits, adapter");
    assert_eq!(stats.rows_rendered, 0);
    assert_eq!(stats.rows_suppressed, 2);
    assert!(!stats.full_inventory);
    assert_eq!(stats.bytes_rendered, delta.len());
    assert_eq!(
        stats.bytes_full_inventory,
        render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP).len()
    );
    assert!(stats.bytes_suppressed() > 0, "{stats:?}");
    // Rendering less freed nothing.
    assert_eq!(table.names(), vec!["hits", "adapter"]);
}

/// (b) A name redeclared this cell renders in full, with its replacement
/// annotation, in a name column of its own; the untouched one is only named.
#[test]
fn a_redeclared_handle_renders_in_full_and_the_rest_do_not() {
    let mut table = two_handles_declared_in_cell_one();
    table.begin_cell(2);
    table.declare("adapter", Value::string("claude-code"), 2);

    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    let header = delta.lines().next().unwrap();
    assert!(
        header.starts_with("adapter  string  (replaced at cell 2)   inline cost"),
        "{header}"
    );
    assert!(delta.contains("\"claude-code\""), "{delta}");
    assert!(
        delta.ends_with("\n\nUnchanged from earlier cells (1): hits"),
        "the closing line is set off by a blank line:\n{delta}"
    );
    assert_eq!((stats.rows_rendered, stats.rows_suppressed), (1, 1));
    assert!(!stats.full_inventory);
}

/// (c) An epilogue recapture with the same preview is not a change; one
/// with a different preview is, and it is stamped with the current cell.
#[test]
fn a_refresh_re_renders_only_when_the_preview_differs() {
    let mut table = two_handles_declared_in_cell_one();
    table.begin_cell(2);
    table.refresh(
        "hits",
        Value::array(vec![Value::Number(1.0), Value::Number(2.0)]),
        HandleMeta::default(),
    );
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(
        delta, "Unchanged from earlier cells (2): hits, adapter",
        "an identical recapture re-rendered the entry"
    );
    assert_eq!(stats.rows_rendered, 0);

    table.begin_cell(3);
    table.refresh(
        "hits",
        Value::array(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(3.0),
        ]),
        HandleMeta::default(),
    );
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(delta.starts_with("hits  Array   n=3   "), "{delta}");
    assert!(delta.contains("[2] 3"), "{delta}");
    assert!(
        delta.ends_with("Unchanged from earlier cells (1): adapter"),
        "{delta}"
    );
    assert_eq!((stats.rows_rendered, stats.rows_suppressed), (1, 1));
    // A refresh reorders nothing and claims no redeclaration.
    assert_eq!(table.names(), vec!["hits", "adapter"]);
    assert!(!delta.contains("replaced at cell"), "{delta}");

    // The next cell: the refreshed entry is unchanged again.
    table.begin_cell(4);
    let (delta, _) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(delta, "Unchanged from earlier cells (2): hits, adapter");
}

/// (d) A pinned entry renders in full every cell until it is unpinned.
#[test]
fn a_pinned_handle_renders_in_full_every_cell() {
    let mut table = two_handles_declared_in_cell_one();
    assert!(table.pin("adapter"));
    for cell in 2..=4 {
        table.begin_cell(cell);
        let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
        assert!(
            delta.starts_with("adapter  string   inline cost"),
            "cell {cell}: the pinned entry was not rendered in full:\n{delta}"
        );
        assert!(delta.contains("\"codex\""), "{delta}");
        assert!(
            delta.ends_with("Unchanged from earlier cells (1): hits"),
            "{delta}"
        );
        assert_eq!((stats.rows_rendered, stats.rows_suppressed), (1, 1));
    }
    assert!(table.unpin("adapter"));
    table.begin_cell(5);
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(delta, "Unchanged from earlier cells (2): hits, adapter");
    assert_eq!(stats.rows_suppressed, 2);
}

/// (e) Over the table cap, full entries are dropped oldest-first, the note
/// keeps its wording, the unchanged line stays, and nothing is freed.
#[test]
fn the_cap_drops_full_entries_and_keeps_the_unchanged_line() {
    let mut table = HandleTable::new();
    table.begin_cell(1);
    table.declare("old0", Value::Number(0.0), 1);
    table.declare("old1", Value::Number(1.0), 1);
    table.begin_cell(2);
    let long = "y".repeat(100);
    for i in 0..5u64 {
        table.declare(format!("new{i}"), Value::string(&long), 2);
    }

    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, 60);

    assert!(
        delta.starts_with("…"),
        "the drop note leads the rendering:\n{delta}"
    );
    assert!(
        delta.contains("older handles not shown; call handles() for the full list"),
        "{delta}"
    );
    assert!(
        delta.ends_with("Unchanged from earlier cells (2): old0, old1"),
        "{delta}"
    );
    assert!(
        delta.contains("new4  string   inline cost"),
        "the newest full entry survives:\n{delta}"
    );
    assert!(
        !delta.contains("new0  string"),
        "the oldest full entry is dropped first:\n{delta}"
    );
    assert_eq!(stats.rows_suppressed, 2, "{stats:?}");
    assert!(stats.rows_rendered < 5, "{stats:?}");
    assert!(!stats.full_inventory);
    for name in ["old0", "old1", "new0", "new1", "new2", "new3", "new4"] {
        assert!(table.is_live(name), "{name} must still be live");
    }
}

/// Past 24 unchanged handles the line names the newest and counts the
/// older ones, so a long task's table stays one bounded line.
#[test]
fn the_unchanged_line_names_the_newest_and_counts_the_rest() {
    let mut table = HandleTable::new();
    table.begin_cell(1);
    for i in 0..30u64 {
        table.declare(format!("h{i}"), Value::Number(i as f64), 1);
    }
    table.begin_cell(2);
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(
        delta.starts_with("Unchanged from earlier cells (30): h6, h7, "),
        "{delta}"
    );
    assert!(
        delta.ends_with(", h29 and 6 older; call handles() for the full list"),
        "{delta}"
    );
    assert!(!delta.contains("h5,"), "{delta}");
    assert_eq!(stats.rows_suppressed, 30);
}

/// The `batch` row is delivered anew every turn it exists, so it is news
/// every time: a producer-rendered entry stays in full while an ordinary
/// old handle is only named beside it. A refresh that carries no label
/// keeps the producer's (`Events.Batch`, never `Object`).
#[test]
fn the_batch_row_stays_in_full_while_old_handles_collapse() {
    let entry = "batch  Events.Batch   1 event\n  bg.done  the build finished".to_string();
    let mut table = HandleTable::new();
    table.begin_cell(1);
    table.declare("hits", Value::Number(1.0), 1);
    table.declare_rendered(
        "batch",
        Value::string(&entry),
        1,
        HandleMeta {
            type_label: Some("Events.Batch".into()),
            size_estimate: entry.len() as u64,
            provenance: None,
        },
        entry.clone(),
    );
    let (delta, _) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(delta.ends_with(&entry), "{delta}");

    table.begin_cell(2);
    // The epilogue recaptures every live name with a label-less meta.
    table.refresh("batch", Value::string(&entry), HandleMeta::default());
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(delta.starts_with(&entry), "{delta}");
    assert!(
        delta.ends_with("Unchanged from earlier cells (1): hits"),
        "{delta}"
    );
    assert!(!delta.contains("Object"), "{delta}");
    assert_eq!(stats.rows_suppressed, 1);
    assert_eq!(stats.rows_rendered, 1);
}
