use pane::contract::{Block, Conversation, Message, Role, SessionId};
use pane::{images, prompt, rollout, wire};
use std::io::{BufRead, BufReader, Read, Write};

const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";

fn root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "pane-image-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn magic_controls_image_type_and_oversized_or_unknown_files_are_rejected() {
    for (bytes, media_type) in [
        (GIF, "image/gif"),
        (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
        ([0xff, 0xd8, 0xff].as_slice(), "image/jpeg"),
        (b"RIFF1234WEBP".as_slice(), "image/webp"),
    ] {
        assert!(
            matches!(images::from_bytes(bytes).unwrap(), Block::Image {media_type: actual, ..} if actual == media_type)
        );
    }
    assert!(images::from_bytes(b"not an image").is_err());
    assert!(
        images::from_bytes(&vec![0; images::MAX_IMAGE_BYTES as usize + 1])
            .unwrap_err()
            .contains("5 MiB")
    );
    let root = root();
    std::fs::write(root.join("misleading.txt"), GIF).unwrap();
    assert!(
        matches!(images::load(&root, std::path::Path::new("misleading.txt")).unwrap(), Block::Image {media_type, ..} if media_type == "image/gif")
    );
    assert!(images::load(&root, std::path::Path::new(".")).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn wire_rollout_and_task_boundary_preserve_attached_image_bytes() {
    let image = images::from_bytes(GIF).unwrap();
    let mut message = Message::text(Role::User, "describe this");
    message.content.push(image.clone());
    let conversation = Conversation {
        system: "test".into(),
        messages: vec![message.clone()],
    };
    let projected = prompt::with_task_context(&conversation, "vision-model", "describe this");
    assert_eq!(projected.messages[0].content[1], image);
    // The projection appends nothing to a message it has already sent — the
    // "[Pane task boundary]" block that used to sit here took the whole
    // prompt cache with it at every task boundary, and the preamble said the
    // same thing anyway (`prompt::with_task_context`).
    assert_eq!(
        projected.messages[0].content.len(),
        2,
        "the request added a block to the person's own message"
    );
    assert_eq!(conversation.messages[0].content.len(), 2);
    let request: serde_json::Value =
        serde_json::from_slice(&wire::request_body(&projected)).unwrap();
    assert_eq!(request["messages"][0]["content"][1]["type"], "image");
    assert_eq!(
        request["messages"][0]["content"][1]["source"]["type"],
        "base64"
    );
    assert_eq!(
        request["messages"][0]["content"][1]["source"]["media_type"],
        "image/gif"
    );
    let root = root();
    let path = root.join("session.jsonl");
    let mut writer = rollout::Rollout::create(&path, SessionId::new("images"), "test").unwrap();
    writer.record_message(&message).unwrap();
    drop(writer);
    assert_eq!(rollout::resume(&path).unwrap().messages[0], message);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_attachment_reaches_provider_and_survives_resume_without_base64_in_machine_events() {
    let root = root();
    std::fs::write(root.join("screen.gif"), GIF).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
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
        let reply = r#"{"role":"assistant","content":[{"type":"text","text":"A tiny image."}]}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pane"))
        .args([
            "exec",
            "describe this",
            "--image",
            "screen.gif",
            "--output-format",
            "json",
            "--model",
            "vision-model",
            "--root",
        ])
        .arg(&root)
        .arg("--rollout")
        .arg(root.join("session.jsonl"))
        .env("ANTHROPIC_BASE_URL", endpoint)
        .env("ANTHROPIC_API_KEY", "test-only")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let source = &request["messages"][0]["content"][1]["source"];
    assert_eq!(source["media_type"], "image/gif");
    assert!(source["data"].as_str().unwrap().starts_with("R0lGODlh"));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["success"], true);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("R0lGODlh"));
    let saved = rollout::resume(&root.join("session.jsonl")).unwrap();
    assert_eq!(
        saved.messages[0].content[1],
        images::from_bytes(GIF).unwrap()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn overflow_checkpoint_retains_current_image_once_and_drops_historic_images() {
    let root = root();
    std::fs::write(root.join("current.gif"), GIF).unwrap();
    let log = root.join("session.jsonl");
    let mut old_message = Message::text(Role::User, "an earlier image task");
    old_message
        .content
        .push(images::from_bytes(b"GIF87a").unwrap());
    let mut writer =
        rollout::Rollout::create(&log, SessionId::new("image-overflow"), "test").unwrap();
    writer.record_message(&old_message).unwrap();
    writer
        .record_turn(Role::Assistant, "Earlier task completed.")
        .unwrap();
    drop(writer);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for turn in 0..2 {
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
            let (status, reply) = if turn == 0 {
                (
                    400,
                    r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#,
                )
            } else {
                (
                    200,
                    r#"{"role":"assistant","content":[{"type":"text","text":"The image survived."}]}"#,
                )
            };
            write!(stream, "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_pane"))
        .args([
            "exec",
            "describe the current image",
            "--image",
            "current.gif",
            "--output-format",
            "json",
            "--model",
            "vision-model",
            "--root",
        ])
        .arg(&root)
        .arg("--rollout")
        .arg(&log)
        .env("ANTHROPIC_BASE_URL", endpoint)
        .env("ANTHROPIC_API_KEY", "test-only")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let initial = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let retry = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let images_in = |request: &serde_json::Value| -> Vec<String> {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .filter(|block| block["type"] == "image")
            .map(|block| block["source"]["data"].as_str().unwrap().to_string())
            .collect()
    };
    let Block::Image {
        data: current_data, ..
    } = images::from_bytes(GIF).unwrap()
    else {
        unreachable!()
    };
    let Block::Image { data: old_data, .. } = images::from_bytes(b"GIF87a").unwrap() else {
        unreachable!()
    };
    let first_images = images_in(&initial);
    assert_eq!(first_images, vec![old_data, current_data.clone()]);
    assert_eq!(images_in(&retry), vec![current_data]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("checkpoint"));
    std::fs::remove_dir_all(root).unwrap();
}
