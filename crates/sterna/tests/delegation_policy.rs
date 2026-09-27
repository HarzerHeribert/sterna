//! Runtime policy acceptance: a picker is not an authority boundary.
use sterna::{
    config::{AgentsMode, SternaConfig},
    contract::SessionId,
    runtime::{isolate::Runtime, outcome::CellOutcome},
    sandbox::profile::Profile,
    wire::Effort,
};
fn config(text: &str) -> SternaConfig {
    SternaConfig::parse(text).unwrap()
}
#[test]
fn unconfigured_and_legacy_auto_never_inherit_main() {
    for c in [SternaConfig::default(), config("[agents]\nmode='auto'")] {
        assert!(c.agents.select(None, None).is_err());
        assert!(c.agents.select(Some("main-model"), None).is_err());
    }
}
#[test]
fn pinned_model_is_an_enforced_choice_not_a_default() {
    let c = config("[agents]\nmode='pinned'\nmodel='chosen-model'");
    assert_eq!(c.agents.select(None, None).unwrap().model, "chosen-model");
    assert_eq!(
        c.agents.select(Some("chosen-model"), None).unwrap().model,
        "chosen-model"
    );
    assert!(c.agents.select(Some("other-model"), None).is_err());
    assert!(c.agents.select(None, Some("quick")).is_err());
}
#[test]
fn populated_slots_do_not_enable_delegation() {
    let c = config("[agents.slots.quick]\nmodel='small-model'");
    assert_eq!(c.agents.mode, AgentsMode::Off);
    assert_eq!(c.agents.slots["quick"].effort, Effort::Low);
    assert!(c.agents.select(None, Some("quick")).is_err());
}
#[test]
fn roster_resolves_only_configured_slots_and_exact_models() {
    let c = config(
        "[agents]\nmode='roster'\n[agents.slots.quick]\nmodel='small-model'\neffort='low'\n[agents.slots.deep]\nmodel='large-model'\neffort='high'",
    );
    assert_eq!(
        c.agents.select(None, Some("deep")).unwrap().model,
        "large-model"
    );
    assert_eq!(
        c.agents.select(Some("small-model"), None).unwrap().effort,
        Effort::Low
    );
    for (m, s) in [
        (None, None),
        (None, Some("heavy")),
        (Some("main-model"), None),
        (Some("large-model"), Some("quick")),
    ] {
        assert!(
            c.agents.select(m, s).is_err(),
            "unapproved assignment: {m:?} {s:?}"
        );
    }
}
#[test]
fn ambiguous_same_model_efforts_require_a_slot() {
    let c = config(
        "[agents]\nmode='roster'\n[agents.slots.quick]\nmodel='same-model'\neffort='low'\n[agents.slots.deep]\nmodel='same-model'\neffort='high'",
    );
    assert!(c.agents.select(Some("same-model"), None).is_err());
    assert_eq!(
        c.agents.select(None, Some("deep")).unwrap().effort,
        Effort::High
    );
}
#[test]
fn invalid_rosters_fail_validation_without_guessing() {
    for text in [
        "[agents]\nmode='roster'",
        "[agents.slots.fast]\nmodel='chosen-model'",
        "[agents.slots.quick]\neffort='low'",
        "[agents.slots.quick]\nmodel='chosen-model'\neffort='default'",
        "[agents.slots.quick]\nmodel='chosen-model'\nprovider='fake-pin'",
    ] {
        assert!(SternaConfig::parse(text).is_err(), "accepted {text}");
    }
}
#[test]
fn captured_roster_does_not_change_when_next_configuration_changes() {
    let mut c = config("[agents]\nmode='roster'\n[agents.slots.quick]\nmodel='first-model'");
    let captured = c.agents.select(None, Some("quick")).unwrap();
    c.agents.slots.get_mut("quick").unwrap().model = "next-model".into();
    assert_eq!(captured.model, "first-model");
    assert_eq!(
        c.agents.select(None, Some("quick")).unwrap().model,
        "next-model"
    );
}
#[test]
fn v8_binding_refuses_unconfigured_delegation_before_spawning() {
    let root = std::env::temp_dir().join(format!("sterna-roster-gate-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let profile = Profile::compile(&root, None);
    let id = SessionId::new("workbench-denied-favorite");
    let mut runtime = Runtime::new(&profile, &id);
    runtime.set_task_context(0, "main-model");
    for code in [
        "agent.run('work');",
        "agent.run('work',{model:'main-model'});",
        "agent.run('work',{slot:'quick',model:'other-model'});",
    ] {
        assert!(matches!(runtime.run_cell(code), CellOutcome::Threw { .. }));
        assert_eq!(sterna::bg::live(&id), 0);
    }
    drop(runtime);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn actual_v8_gate_rejects_model_and_effort_escape_from_a_populated_roster() {
    let root = std::env::temp_dir().join(format!("sterna-roster-exact-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let profile = Profile::compile(&root, None);
    let id = SessionId::new("workbench-roster-denials");
    let mut runtime = Runtime::new(&profile, &id).with_agents(
        config("[agents]\nmode='roster'\n[agents.slots.quick]\nmodel='small-model'\neffort='low'")
            .agents,
    );
    runtime.set_task_context(0, "main-model");
    for code in [
        "agent.run('work',{slot:'heavy'});",
        "agent.run('work',{slot:'quick',model:'main-model'});",
        "agent.run('work',{slot:'quick',effort:'high'});",
    ] {
        assert!(matches!(runtime.run_cell(code), CellOutcome::Threw { .. }));
        assert_eq!(sterna::bg::live(&id), 0, "a denied job was started");
    }
    drop(runtime);
    let _ = std::fs::remove_dir_all(root);
}
