use std::io::{BufRead, BufReader, Read, Write};
use sterna::contract::SessionId;
use sterna::project::agents::{Catalog, Definition};
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::sandbox::profile::Profile;

/// `ANTHROPIC_BASE_URL` is process-global, and `setenv` on macOS may
/// reallocate the environment block while another thread is inside
/// `getenv` -- which every `Fixture::new` here is, by way of
/// `std::env::temp_dir`. So the one test that points the variable at a
/// fixture takes this lock for writing and every other test takes it for
/// reading, which is the same serialisation `wire.rs` and `tests/helpers.rs`
/// already use. Four flaky reds in the week to 2026-09-19 were this and the
/// fixture-name collision below; both are removed rather than re-run.
/// Poisoning is stepped over deliberately: one failing test here must
/// report one red, not six.
static ENV_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());

struct Fixture(std::path::PathBuf);
impl Fixture {
    /// A counter, not a clock. `SystemTime::now().as_nanos()` names
    /// nanoseconds but does not resolve them, so two fixtures built in the
    /// same microsecond shared a directory -- and then one test's rewrite
    /// landed in another's snapshot.
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sterna-custom-agents-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".sterna/agents")).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(format!(".sterna/agents/{name}.toml")), text).unwrap();
    }
    fn profile(&self) -> Profile {
        Profile::compile(&self.0, Some(r#"{"permissions":{"allow":["Read(**)"]}}"#))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn definitions_cannot_grant_permissions_and_validate_routing_defaults() {
    let _env = ENV_LOCK.read().unwrap_or_else(|poison| poison.into_inner());
    for text in [
        "instructions='Review'\npermissions=['Bash(*)']",
        "instructions='Review'\nmodel='off'",
        "instructions='Review'\neffort='enormous'",
        "instructions=''",
        "model='model'",
    ] {
        assert!(Definition::parse(text).is_err(), "accepted {text}");
    }
    let definition = Definition::parse(
        "instructions='Review correctness'\nmodel='provider/model'\neffort='xhigh'",
    )
    .unwrap();
    assert_eq!(definition.effort, Some(sterna::wire::Effort::Xhigh));
    assert!(definition.task("inspect parser").contains("inspect parser"));
}

#[test]
fn catalog_is_a_permission_checked_immutable_snapshot() {
    let _env = ENV_LOCK.read().unwrap_or_else(|poison| poison.into_inner());
    let fixture = Fixture::new();
    fixture.write("review", "instructions='original'\neffort='high'");
    let catalog = Catalog::load(&fixture.profile());
    fixture.write("review", "instructions='changed'");
    assert_eq!(catalog.resolve("review").unwrap().instructions, "original");
    assert!(catalog.resolve("../review").is_err());
    assert!(catalog.resolve("absent").is_err());
    let denied = Profile::compile(
        &fixture.0,
        Some(r#"{"permissions":{"allow":["Read(**)"],"deny":["Read(.sterna/agents/**)"]}}"#),
    );
    assert!(
        Catalog::load(&denied)
            .resolve("review")
            .unwrap_err()
            .contains("denied")
    );
}

#[cfg(unix)]
#[test]
fn external_symlink_definition_is_refused() {
    let _env = ENV_LOCK.read().unwrap_or_else(|poison| poison.into_inner());
    let fixture = Fixture::new();
    let outside = Fixture::new();
    outside.write("external", "instructions='outside'");
    std::os::unix::fs::symlink(
        outside.0.join(".sterna/agents/external.toml"),
        fixture.0.join(".sterna/agents/link.toml"),
    )
    .unwrap();
    assert!(
        Catalog::load(&fixture.profile())
            .resolve("link")
            .unwrap_err()
            .contains("escapes")
    );
}

#[test]
fn invalid_profile_throws_before_a_background_agent_is_created() {
    let _env = ENV_LOCK.read().unwrap_or_else(|poison| poison.into_inner());
    let fixture = Fixture::new();
    let id = SessionId::new("invalid-custom-profile");
    let mut runtime = Runtime::new(&fixture.profile(), &id);
    runtime.set_task_context(0, "test/model");
    assert!(matches!(
        runtime.run_cell("agent.run('review', {profile:'unknown'});"),
        CellOutcome::Threw { .. }
    ));
    assert_eq!(sterna::bg::live(&id), 0);
}

#[test]
fn named_agent_routes_snapshot_instructions_model_and_effort_with_explicit_override() {
    let _env = ENV_LOCK
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join(".glasshouse")).unwrap();
    std::fs::write(
        fixture.0.join(".glasshouse/pane.toml"),
        "[helpers]\nacceptance_list = false\nmodel='base-helper'\nenabled=true\n",
    )
    .unwrap();
    fixture.write(
        "review",
        "instructions='ORIGINAL_REVIEW_GUIDANCE'\nmodel='template-model'\neffort='high'",
    );
    let id = SessionId::new("custom-agent-routing");
    let mut effective = sterna::config::SternaConfig::load(&fixture.0).unwrap();
    // A decision model the project file does not name: a child that binds
    // `decide` read the effective configuration, not the file.
    effective.decisions.model = Some("jev-latest".into());
    // Explicitly authorize both fixture assignments; templates are not authority.
    effective.agents = sterna::config::SternaConfig::parse("[agents]\nmode='roster'\n[agents.slots.deep]\nmodel='template-model'\neffort='high'\n[agents.slots.quick]\nmodel='explicit-model'\neffort='low'").unwrap().agents;
    let mut runtime = Runtime::new(&fixture.profile(), &id)
        .with_config(effective)
        .unwrap();
    runtime.set_task_context(0, "parent-model");
    fixture.write(
        "review",
        "instructions='MUTATED_GUIDANCE'\nmodel='mutated-model'",
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (sender, requests) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            sender
                .send(serde_json::from_slice::<serde_json::Value>(&body).unwrap())
                .unwrap();
            let response = r#"{"role":"assistant","content":[{"type":"tool_use","id":"check-helpers","name":"execute_cell","input":{"code":"answer('decide is ' + typeof decide);"}}]}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
    });
    let previous = std::env::var_os("ANTHROPIC_BASE_URL");
    // SAFETY: `ENV_LOCK` is held for writing, so no other test in this
    // process is inside `getenv` while this runs, and restoration follows
    // agent thread teardown.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", endpoint);
    }
    let first = runtime.run_cell("agent.run('inspect parser', {profile:'review'});");
    let second = runtime.run_cell(
        "agent.run('inspect parser', {profile:'review', model:'explicit-model', effort:'low'});",
    );
    let received = [
        requests.recv_timeout(std::time::Duration::from_secs(20)),
        requests.recv_timeout(std::time::Duration::from_secs(20)),
    ];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while sterna::bg::live(&id) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let events = sterna::bg::drain(&id);
    let answers: Vec<_> = events
        .iter()
        .filter_map(|event| sterna::bg::payload(&id, event.payload.as_str()))
        .collect();
    sterna::bg::shutdown(&id);
    unsafe {
        match previous {
            Some(value) => std::env::set_var("ANTHROPIC_BASE_URL", value),
            None => std::env::remove_var("ANTHROPIC_BASE_URL"),
        }
    }
    assert!(!matches!(first, CellOutcome::Threw { .. }));
    assert!(!matches!(second, CellOutcome::Threw { .. }));
    assert_eq!(answers.len(), 2);
    assert!(
        answers
            .iter()
            .all(|answer| answer.stdout.contains("decide is object")),
        "the child ignored the effective config: {answers:?}"
    );
    for request in received {
        let request = request.unwrap();
        let serialized = request.to_string();
        assert!(serialized.contains("ORIGINAL_REVIEW_GUIDANCE"));
        assert!(!serialized.contains("MUTATED_GUIDANCE"));
        assert!(serialized.contains("inspect parser"));
        match request["model"].as_str().unwrap() {
            "template-model" => assert_eq!(request["output_config"]["effort"], "high"),
            "explicit-model" => assert_eq!(request["output_config"]["effort"], "low"),
            unexpected => panic!("unexpected model {unexpected}"),
        }
    }
    server.join().unwrap();
}

/// A definition file past the catalogue's own limit is reported, not turned
/// into "define this file" advice about a file that already exists.
#[test]
fn a_definition_past_the_catalogue_limit_is_named_as_unloaded_not_as_missing() {
    let _env = ENV_LOCK.read().unwrap_or_else(|poison| poison.into_inner());
    let fixture = Fixture::new();
    for index in 0..129 {
        fixture.write(
            &format!("agent{index:03}"),
            "instructions = \"do the thing\"\n",
        );
    }
    let catalog = Catalog::load(&fixture.profile());
    assert_eq!(
        catalog.omitted(),
        1,
        "one file past the 128-definition limit was not loaded"
    );
    let error = catalog
        .resolve("agent128")
        .expect_err("the last file is past the limit, so it did not load");
    assert!(
        error.contains("were not loaded") && error.contains("at most 128"),
        "the refusal says the file exists but was not loaded, rather than telling the person to \
         create it: {error}"
    );
}
