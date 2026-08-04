// Probe the native session: init, then one real chat turn against the
// configured target (MARC27 cloud on this machine). Prints protocol
// values as they arrive. PRISM_OFFLINE=1 skips the chat turn.

use std::time::{Duration, Instant};

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let session = prism_frontend::spawn_native_session(std::path::Path::new(&root))
        .expect("native session should resolve");
    let (tx, rx) = session.into_parts();

    let req = |method: &str, params: serde_json::Value| {
        tx.send(
            serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0", "method": method, "params": params
            }))
            .unwrap(),
        )
        .unwrap();
    };

    req(
        "init",
        serde_json::json!({"auto_approve": false, "resume": ""}),
    );

    let start = Instant::now();
    let mut turn_sent = false;
    let mut turn_done = false;
    while start.elapsed() < Duration::from_secs(60) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(v) => {
                let m = v.get("method").and_then(|m| m.as_str()).unwrap_or("(resp)");
                match m {
                    "ui.welcome" => println!("[welcome] {}", v["params"]),
                    "ui.status" => println!(
                        "[status] model={} mode={}",
                        v["params"]["model"], v["params"]["mode"]
                    ),
                    "ui.text.delta" => {
                        print!("{}", v["params"]["text"].as_str().unwrap_or(""));
                        std::io::Write::flush(&mut std::io::stdout()).unwrap();
                    }
                    "ui.thinking.delta" => print!("·"),
                    "ui.tool.start" => println!(
                        "\n[tool] {} {}",
                        v["params"]["tool_name"], v["params"]["verb"]
                    ),
                    "ui.card" => println!("\n[card] {} :: {}", v["params"]["tool_name"], {
                        let c = v["params"]["content"].as_str().unwrap_or("");
                        &c[..c
                            .char_indices()
                            .take(120)
                            .last()
                            .map(|(i, _)| i)
                            .unwrap_or(0)]
                    }),
                    "ui.turn.complete" => {
                        println!("\n[turn.complete]");
                        turn_done = true;
                    }
                    "ui.backend.error" => println!("\n[error] {}", v["params"]),
                    "ui.backend.warning" => println!("\n[warn] {}", v["params"]),
                    "(resp)" => println!("[resp] id={}", v["id"]),
                    other => println!("[{}] ", other),
                }
                if !turn_sent
                    && m == "ui.welcome"
                    && std::env::var("PRISM_OFFLINE").as_deref() != Ok("1")
                {
                    req(
                        "input.message",
                        serde_json::json!({"text": "Reply with exactly the single word: prism-ok"}),
                    );
                    turn_sent = true;
                }
                if turn_done {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    drop(tx);
    println!("\nprobe done (turn_sent={turn_sent}, turn_done={turn_done})");
}
