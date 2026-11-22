//! The console's AI Agents section, drawn as HTML from the index's rows: what waits for the person
//! (questions and passkeys), then the runs, live first. Every value is escaped. Pure.

use serde_json::Value;

/// `s` with HTML's special characters escaped.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => esc(s),
        Value::Null => String::new(),
        other => esc(&other.to_string()),
    }
}

/// What a waiting run waits for, in words.
fn waits_for(w: &Value) -> String {
    match w["for"].as_str() {
        Some("answer") => {
            let options: Vec<String> = w["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect();
            let offered = if options.is_empty() {
                String::new()
            } else {
                format!(" <span class=\"muted\">({})</span>", options.join(" · "))
            };
            format!("asks: <q>{}</q>{offered}", text(&w["question"]))
        }
        Some("approval") => format!(
            "waits for your passkey on approval <code>{}</code>, on the Approvals tab",
            text(&w["id"])
        ),
        Some("executors") => "waits for its executors".into(),
        Some("provider") => "waits for the model provider, which keeps failing".into(),
        _ => String::new(),
    }
}

/// The section's HTML from `{paused, runs}` as the index answers it.
pub fn section(data: &Value) -> String {
    let runs: Vec<&Value> = data["runs"].as_array().into_iter().flatten().collect();
    let mut out = String::new();
    if data["paused"] == true {
        out.push_str(
            "<p class=\"c-fail\">Every run is paused: none takes a step until \
             <code>ai_agents_resume</code>.</p>",
        );
    }
    let waiting: Vec<&&Value> = runs
        .iter()
        .filter(|r| r["state"] == "waiting" && r["waiting"].is_object())
        .collect();
    if !waiting.is_empty() {
        out.push_str("<div class=\"card\"><h3>Waiting for you</h3><ul>");
        for r in waiting {
            out.push_str(&format!(
                "<li><code>{}</code> {}. Answer in your Claude chat, or with \
                 <code>ai_agents_answer</code>.</li>",
                text(&r["run"]),
                waits_for(&r["waiting"])
            ));
        }
        out.push_str("</ul></div>");
    }
    let assumed: Vec<String> = runs
        .iter()
        .flat_map(|r| {
            r["assumed"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|q| format!("<li><code>{}</code> assumed an answer to <q>{}</q>; its report says what it chose.</li>", text(&r["run"]), text(&q["question"])))
                .collect::<Vec<_>>()
        })
        .collect();
    if !assumed.is_empty() {
        out.push_str("<div class=\"card\"><h3>Went on without your answer</h3><p class=\"muted\">No answer came within 5 minutes, so each run made the conventional choice.</p><ul>");
        for line in assumed {
            out.push_str(&line);
        }
        out.push_str("</ul></div>");
    }
    if runs.is_empty() {
        out.push_str(
            "<p class=\"muted\">No runs yet. Your Claude chat starts them with \
             <code>ai_agents_start</code>, unless you ask for the work to be done in the chat.</p>",
        );
        return out;
    }
    out.push_str(
        "<table><thead><tr><th>Run</th><th>Role</th><th>Where</th><th>State</th>\
         <th>Now</th><th>Spent</th><th>Branch</th><th>Brief</th></tr></thead><tbody>",
    );
    for r in &runs {
        let brief: String = r["brief"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect();
        let spent = format!(
            "{} · {} turns",
            r["spent"]["cost"].as_str().unwrap_or("$0"),
            r["spent"]["turns"].as_u64().unwrap_or(0)
        );
        let where_ = if r["runtime"] == "claude_code" {
            "Claude chat"
        } else {
            "gateway"
        };
        let now = if r["state"] == "done" || r["state"] == "failed" {
            r["report"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect::<String>()
        } else {
            r["step"].as_str().unwrap_or_default().to_string()
        };
        out.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
             <td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
            text(&r["run"]),
            text(&r["role"]),
            where_,
            text(&r["state"]),
            esc(&now),
            esc(&spent),
            text(&r["branch"]),
            esc(&brief)
        ));
    }
    out.push_str("</tbody></table>");
    out.push_str(
        "<p class=\"muted\">Stop a run with <code>ai_agents_stop</code>, every run with \
         <code>ai_agents_pause</code>. Cost per run: <code>models_usage</code> with \
         <code>task_prefix: ai-agent/</code>.</p>",
    );
    out
}
