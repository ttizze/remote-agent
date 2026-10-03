use super::server::{Context, SharedThread, Turn};
use crate::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{cell::RefCell, fs, io::BufWriter, path::Path, rc::Rc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(super) async fn wait_for_release(home: &Path, stop: &CancellationToken) -> bool {
    let path = home.join("release-inputs");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline && !stop.is_cancelled() {
        if path.exists() {
            return true;
        }
        if interrupted(stop, Duration::from_millis(20)).await {
            break;
        }
    }
    false
}

async fn interrupted(stop: &CancellationToken, delay: Duration) -> bool {
    tokio::select! { biased; _ = stop.cancelled() => true, _ = tokio::time::sleep(delay) => false }
}

fn complete(context: &Context, thread_id: &str, turn: &Turn, index: usize) -> Result<()> {
    let turn = turn.borrow();
    context.item_event(
        "item/completed",
        thread_id,
        turn["id"].as_str().unwrap(),
        &turn["items"][index],
    )
}

pub(super) async fn run(
    context: Rc<Context>,
    thread: SharedThread,
    turn: Turn,
    mut inputs: Vec<Value>,
    suffix: String,
    stop: CancellationToken,
    client_id: Value,
) -> Result<()> {
    let prompt = inputs
        .iter()
        .find(|item| item["type"] == "text")
        .and_then(|item| item["text"].as_str())
        .unwrap_or("")
        .to_owned();
    let thread_id = thread.borrow().metadata["id"].as_str().unwrap().to_owned();
    let turn_id = turn.borrow()["id"].as_str().unwrap().to_owned();
    let lower = prompt.to_lowercase();
    let scenario = [
        "retry",
        "failed",
        "interrupted",
        "request",
        "approval",
        "items",
        "history",
        "duplicate",
        "followups",
    ]
    .into_iter()
    .find(|name| lower.contains(&format!("[{name}]")))
    .unwrap_or("success");
    if scenario == "history" {
        thread
            .borrow_mut()
            .metadata
            .insert("name".into(), "History conversation".into());
    }
    let image = if prompt.contains("[images]") || prompt.contains("[generated-images]") {
        let path = context.home.join("fixture image.png");
        let bytes = include_bytes!(
            "../../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
        );
        fs::write(&path, bytes)?;
        if prompt.contains("[images]") {
            inputs.push(json!({"type":"localImage","path":path}));
        }
        Some((
            path,
            format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
        ))
    } else {
        None
    };
    if prompt.contains("[gallery]") {
        gallery(&context.home, &thread)?;
    }

    context.turn_event("turn/started", &thread_id, &turn.borrow())?;
    // Native Codex advertises a rollout path before history is materialized.
    // A file event is not evidence that thread/turns/list can hydrate it yet.
    if let Some(path) = thread.borrow().metadata.get("path").and_then(Value::as_str) {
        fs::write(path, "fixture rollout initializing\n")?;
    }
    if prompt.contains("[delayed-input]") && !wait_for_release(&context.home, &stop).await {
        return context.finish(&thread, &turn, "interrupted", None);
    }
    context.stream_item(
        &thread_id,
        &turn,
        json!({"id":format!("fixture-user-{suffix}"),"clientId":client_id,
        "type":"userMessage","text":prompt,"content":inputs}),
    )?;
    context.stream_item(
        &thread_id,
        &turn,
        json!({"id":format!("fixture-commentary-{suffix}"),"type":"agentMessage",
        "text":"シミュレータで確認しています…","phase":"commentary"}),
    )?;
    context.stream_item(
        &thread_id,
        &turn,
        json!({"id":format!("fixture-reasoning-{suffix}"),"type":"reasoning","summary":["確認中"]}),
    )?;
    let command_index = turn.borrow()["items"].as_array().unwrap().len();
    context.stream_item(&thread_id, &turn, json!({"id":format!("fixture-command-{suffix}"),"type":"commandExecution",
        "command":"./gradlew test","cwd":thread.borrow().metadata["cwd"],"aggregatedOutput":"running","status":"inProgress"}))?;

    match scenario {
        "retry" => context.notify("error", &json!({"threadId":thread_id,"turnId":turn_id,"willRetry":true,
            "error":{"message":"stream disconnected","additionalDetails":"attempt 2 of 5","codexErrorInfo":{"responseStreamDisconnected":{"httpStatusCode":429}}}}))?,
        "request" | "approval" => {
            let mut params = json!({"threadId":thread_id,"turnId":turn_id,"itemId":format!("fixture-command-{suffix}")});
            let method = if scenario == "request" {
                params["questions"] = json!([{"id":"continue","header":"継続","question":"このまま続けますか？",
                    "options":[{"label":"続ける","description":"作業を進めます"},{"label":"見直す","description":"方針を見直します"}],"isOther":true,"isSecret":false}]);
                "item/tool/requestUserInput"
            } else {
                params["command"] = "echo fixture".into();
                params["cwd"] = thread.borrow().metadata["cwd"].clone();
                params["reason"] = "結合テストの承認確認".into();
                "item/commandExecution/requestApproval"
            };
            let (request_id, reply) = context.request(method, &params)?;
            let response = tokio::select! {
                _ = stop.cancelled() => Value::Null,
                result = reply => result.unwrap_or(Value::Null),
            };
            context.notify("serverRequest/resolved", &json!({"threadId":thread_id,"requestId":request_id}))?;
            if stop.is_cancelled() || (scenario == "approval" && !matches!(response["decision"].as_str(), Some("accept" | "acceptForSession"))) {
                return context.finish(&thread, &turn, "interrupted", None);
            }
        }
        "items" => {
            for item in [
                json!({"id":format!("fixture-plan-{suffix}"),"type":"plan","text":"確認計画"}),
                json!({"id":format!("fixture-mcp-{suffix}"),"type":"mcpToolCall","server":"fixture","tool":"lookup","status":"completed","result":{"content":[{"type":"text","text":"Fixture lookup result"}]}}),
                json!({"id":format!("fixture-dynamic-{suffix}"),"type":"dynamicToolCall","tool":"fixture","status":"inProgress"}),
                json!({"id":format!("fixture-collab-{suffix}"),"type":"collabAgentToolCall","tool":"spawn_agent","status":"inProgress"}),
                json!({"id":format!("fixture-subagent-{suffix}"),"type":"subAgentActivity","status":"running"}),
                json!({"id":format!("fixture-web-{suffix}"),"type":"webSearch","query":"Codex"}),
                json!({"id":format!("fixture-image-{suffix}"),"type":"imageView","path":"/tmp/fixture.png"}),
                json!({"id":format!("fixture-sleep-{suffix}"),"type":"sleep"}),
                json!({"id":format!("fixture-review-in-{suffix}"),"type":"enteredReviewMode"}),
                json!({"id":format!("fixture-review-out-{suffix}"),"type":"exitedReviewMode"}),
                json!({"id":format!("fixture-compaction-{suffix}"),"type":"contextCompaction"}),
            ] { context.stream_item(&thread_id, &turn, item)?; }
        }
        _ => (),
    }
    if interrupted(&stop, context.config.delay()).await {
        return context.finish(&thread, &turn, "interrupted", None);
    }
    {
        let mut turn = turn.borrow_mut();
        let command = &mut turn["items"][command_index];
        command["aggregatedOutput"] = if scenario == "history" {
            super::history::detail_output().into()
        } else {
            "passed".into()
        };
        command["status"] = "completed".into();
        command["exitCode"] = 0.into();
    }
    complete(&context, &thread_id, &turn, command_index)?;
    if prompt.contains("[workspace-edit]") {
        let cwd = thread
            .borrow()
            .metadata
            .get("readCwd")
            .unwrap_or(&thread.borrow().metadata["cwd"])
            .as_str()
            .ok_or("workspace edit fixture requires a working directory")?
            .to_owned();
        fs::write(
            Path::new(&cwd).join("tracked.txt"),
            "Edited during the turn\n",
        )?;
        fs::write(
            Path::new(&cwd).join("added.txt"),
            "New first\nNew second\nNew third\n",
        )?;
        let index = turn.borrow()["items"].as_array().unwrap().len();
        context.stream_item(
            &thread_id,
            &turn,
            json!({
                "id":format!("fixture-edit-{suffix}"),"type":"fileChange","status":"completed",
                "changes":[{"path":"tracked.txt","kind":{"type":"update"},
                    "diff":"-original\n+Edited during the turn\n"},
                    {"path":"added.txt","kind":{"type":"add"},
                    "diff":"+New first\n+New second\n+New third\n"}]
            }),
        )?;
        complete(&context, &thread_id, &turn, index)?;
        if !wait_for_release(&context.home, &stop).await {
            return context.finish(&thread, &turn, "interrupted", None);
        }
    }
    if prompt.contains("[groups]") {
        context.stream_item(
            &thread_id,
            &turn,
            json!({"id":format!("fixture-progress-{suffix}"),"type":"agentMessage",
            "text":"最初の確認が終わりました。次のコマンドを確認します。","phase":"commentary"}),
        )?;
        context.stream_item(&thread_id, &turn, json!({"id":format!("fixture-next-command-{suffix}"),"type":"commandExecution",
            "command":"pwd","aggregatedOutput":"GROUP_DETAIL_OUTPUT","status":"completed","exitCode":0}))?;
        if !wait_for_release(&context.home, &stop).await {
            return context.finish(&thread, &turn, "interrupted", None);
        }
    }
    if scenario == "failed" {
        let error = json!({"message":"context is full","codexErrorInfo":"contextWindowExceeded"});
        context.notify(
            "error",
            &json!({"threadId":thread_id,"turnId":turn_id,"willRetry":false,"error":error}),
        )?;
        return context.finish(&thread, &turn, "failed", Some(error));
    }
    if scenario == "interrupted" {
        return context.finish(&thread, &turn, "interrupted", None);
    }
    if scenario == "followups" {
        for item in [
            json!({"id":"history-answer-1","type":"agentMessage","text":"Earlier answer remains visible."}),
            json!({"id":"history-followup-1","type":"userMessage","content":[{"type":"text","text":"Next question"}]}),
            json!({"id":"history-answer-2","type":"agentMessage","phase":"commentary","text":"Reply before the next instruction."}),
            json!({"id":"history-followup-2","type":"userMessage","content":[{"type":"text","text":"One more question"}]}),
        ] {
            let index = turn.borrow()["items"].as_array().unwrap().len();
            context.stream_item(&thread_id, &turn, item)?;
            complete(&context, &thread_id, &turn, index)?;
        }
    }
    let mut response_text = if prompt.contains("[selection]") {
        "Needle Alpha Bravo.\n\nSecond paragraph stays unselected.".to_owned()
    } else if prompt.contains("[visualize]") {
        let path = context.home.join(format!("icon-options-{suffix}.html"));
        fs::write(
            &path,
            include_str!("../../../agent-core/tests/fixtures/visualize/icon-options.html"),
        )?;
        format!("visualize{}", json!({"path": path}))
    } else if prompt.contains("[markdown-table]") {
        include_str!("../../../agent-core/tests/fixtures/markdown/table.md").to_owned()
    } else if scenario == "history" {
        "ローカル relay 構成で Mac・iPhone アプリの実装と検証を完了しました。\n\n- **Mac アプリ**：会話、リモート操作、添付・保存・差分を確認。\n- iPhone Simulator：**10/10 成功、スキップ 0**。\n- SwiftUI の会話表示と入力欄を更新しました。\n\n変更したファイルは、下の差分から確認できます。".to_owned()
    } else {
        "シミュレータで完了しました。".to_owned()
    };
    if let Some((path, url)) = &image {
        response_text = format!(
            "Hostの画像です。\n\n![Hostから読み込んだ画像](<{}>)\n\nインライン画像です。\n\n![インライン画像]({url})",
            path.display()
        );
        if prompt.contains("[generated-images]") {
            for (name, saved) in [("saved", Some(path)), ("inline", None)] {
                let index = turn.borrow()["items"].as_array().unwrap().len();
                context.stream_item(&thread_id, &turn, json!({"id":format!("fixture-generated-{name}-{suffix}"),"type":"imageGeneration","status":"inProgress","result":"","savedPath":null}))?;
                {
                    let mut turn = turn.borrow_mut();
                    let item = &mut turn["items"][index];
                    item["status"] = "completed".into();
                    item["savedPath"] = serde_json::to_value(saved)?;
                    item["result"] = url.split_once(',').unwrap().1.into();
                }
                complete(&context, &thread_id, &turn, index)?;
            }
            response_text = format!(
                "生成画像を表示しました。\n\n[生成画像を開く](<{}>)\n\n[ファイルを開く](hello.txt:1)\n\n[Webサイトを開く](https://example.com)",
                path.display()
            );
        }
    }
    let final_id = format!("fixture-final-{suffix}");
    let index = turn.borrow()["items"].as_array().unwrap().len();
    context.stream_item(
        &thread_id,
        &turn,
        json!({"id":final_id,"type":"agentMessage","text":"","phase":"final_answer"}),
    )?;
    if prompt.contains("[long-markdown]") {
        let body = "Markdown stream fixture ".repeat(80) + "\n";
        for chunk in std::iter::once("```text\n")
            .chain(std::iter::repeat_n(body.as_str(), 36))
            .chain(std::iter::once("\n```\n\n**MARKDOWN_STREAM_COMPLETE**"))
        {
            if interrupted(&stop, Duration::from_millis(250)).await {
                return Ok(());
            }
            {
                let mut turn = turn.borrow_mut();
                if let Value::String(text) = &mut turn["items"][index]["text"] {
                    text.push_str(chunk);
                }
            }
            context.notify(
                "item/agentMessage/delta",
                &json!({"threadId":thread_id,"turnId":turn_id,"itemId":final_id,"delta":chunk}),
            )?;
        }
    } else {
        context.notify(
            "item/agentMessage/delta",
            &json!({"threadId":thread_id,"turnId":turn_id,"itemId":final_id,"delta":response_text}),
        )?;
        turn.borrow_mut()["items"][index]["text"] = response_text.into();
    }
    complete(&context, &thread_id, &turn, index)?;
    context.finish(&thread, &turn, "completed", None)?;
    if scenario == "duplicate" {
        thread.borrow_mut().turns.push(Rc::new(RefCell::new(json!({"id":turn_id,"status":"completed","items":[
            {"id":"duplicate-history-old","type":"agentMessage","phase":"final_answer","text":"Older AI response must remain visible."}]}))));
    }
    Ok(())
}

fn gallery(home: &Path, thread: &SharedThread) -> Result<()> {
    let mut older = Vec::with_capacity(6);
    let mut pixels = vec![0; 128 * 128 * 3];
    for index in 0..6 {
        let color = [35 + index * 32, 55 + index * 20, 180 - index * 22];
        for pixel in pixels.as_chunks_mut::<3>().0 {
            pixel.copy_from_slice(&color);
        }
        let path = home.join(format!("gallery-{index}.png"));
        let mut encoder = png::Encoder::new(BufWriter::new(fs::File::create(&path)?), 128, 128);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&pixels)?;
        writer.finish()?;
        let mut items = vec![
            json!({"id":format!("gallery-image-{index}"),"type":"imageGeneration","status":"completed","savedPath":path,"result":""}),
        ];
        if index == 0 {
            items.extend((0..600).map(|number| json!({"id":format!("gallery-command-{number}"),"type":"commandExecution","status":"completed","command":"inspect","aggregatedOutput":"done"})));
        }
        older.push(Rc::new(RefCell::new(
            json!({"id":format!("gallery-old-{index}"),"status":"completed","items":items}),
        )));
    }
    let mut thread = thread.borrow_mut();
    thread
        .metadata
        .insert("historyMode".into(), "paginated".into());
    thread.metadata.insert("gallery".into(), true.into());
    thread.turns.splice(..0, older);
    Ok(())
}
