//! Dependency-free subprocess sink for the relay process-kill drill.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::time::Duration;

fn request_id(line: &str) -> Option<u64> {
    let rest = line.split_once("\"id\"")?.1;
    let value = rest.split_once(':')?.1.trim_start();
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let sink_file = arguments.next().expect("sink file argument");
    let hold_ms = arguments
        .next()
        .expect("hold milliseconds argument")
        .parse::<u64>()
        .expect("hold milliseconds are numeric");
    let input = std::io::stdin();
    let mut output = std::io::stdout().lock();
    for line in input.lock().lines() {
        let Ok(line) = line else { break };
        if line.contains("\"shutdown\"") {
            break;
        }
        let Some(id) = request_id(&line) else {
            continue;
        };
        let response = if line.contains("tool.call") && line.contains("discord.send_message") {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&sink_file)
                .expect("open disposable sink file");
            writeln!(file, "accepted:discord.send_message").expect("append accepted effect");
            file.sync_all().expect("persist accepted effect before ack");
            std::thread::sleep(Duration::from_millis(hold_ms));
            format!("{{\"id\":{id},\"result\":{{\"message_id\":\"qa-sink-accepted\"}}}}")
        } else {
            format!("{{\"id\":{id},\"error\":{{\"code\":-32601,\"message\":\"unknown method\"}}}}")
        };
        if writeln!(output, "{response}").is_err() || output.flush().is_err() {
            break;
        }
    }
}
