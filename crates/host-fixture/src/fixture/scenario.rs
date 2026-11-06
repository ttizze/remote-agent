use super::server::{Context, SharedThread, Turn};
use crate::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
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

pub(super) struct ScenarioTurn {
    pub context: Rc<Context>,
    pub thread: SharedThread,
    pub turn: Turn,
    pub inputs: Vec<Value>,
    pub suffix: String,
    pub stop: CancellationToken,
    pub client_id: Value,
}

#[derive(Debug, PartialEq)]
enum RequestOutcome {
    Continue,
    Interrupted,
    Failed(Value),
}

fn expected_question_answer(prompt: &str) -> Option<Value> {
    if prompt.contains("[choice]") {
        Some(json!({"answers":{"continue":{"answers":["続ける"]}}}))
    } else if prompt.contains("[skip]") {
        Some(json!({"answers":{}}))
    } else {
        None
    }
}

fn request_result(
    prompt: &str,
    scenario: &str,
    response: &Value,
    interrupted: bool,
) -> RequestOutcome {
    if interrupted
        || (scenario == "approval"
            && !matches!(
                response["decision"].as_str(),
                Some("accept" | "acceptForSession")
            ))
    {
        return RequestOutcome::Interrupted;
    }
    if scenario == "request"
        && expected_question_answer(prompt).is_some_and(|expected| response != &expected)
    {
        return RequestOutcome::Failed(json!({"message":"Unexpected question answer"}));
    }
    RequestOutcome::Continue
}

impl ScenarioTurn {
    pub(super) async fn run(mut self) -> Result<()> {
        let context = &self.context;
        let thread = &self.thread;
        let turn = &self.turn;
        let inputs = &mut self.inputs;
        let suffix = &self.suffix;
        let stop = &self.stop;
        let client_id = &self.client_id;
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
        let attach_image = prompt.contains("[images]");
        let image = prepare_scenario_image(&context.home, &prompt)?;
        if attach_image && let Some((path, _)) = &image {
            inputs.push(json!({"type":"localImage","path":path}));
        }
        if prompt.contains("[gallery]") {
            gallery(&context.home, thread)?;
        }

        context.turn_event("turn/started", &thread_id, &turn.borrow())?;
        // Native Codex advertises a rollout path before history is materialized.
        // A file event is not evidence that thread/turns/list can hydrate it yet.
        if let Some(path) = thread.borrow().metadata.get("path").and_then(Value::as_str) {
            fs::write(path, "fixture rollout initializing\n")?;
        }
        if prompt.contains("[delayed-input]") && !wait_for_release(&context.home, stop).await {
            return context.finish(thread, turn, "interrupted", None);
        }
        context.stream_item(
            &thread_id,
            turn,
            json!({"id":format!("fixture-user-{suffix}"),"clientId":client_id,
        "type":"userMessage","text":prompt,"content":inputs}),
        )?;
        context.stream_item(
            &thread_id,
            turn,
            json!({"id":format!("fixture-commentary-{suffix}"),"type":"agentMessage",
        "text":"シミュレータで確認しています…","phase":"commentary"}),
        )?;
        context.stream_item(
        &thread_id,
        turn,
        json!({"id":format!("fixture-reasoning-{suffix}"),"type":"reasoning","summary":["確認中"]}),
    )?;
        let command_index = turn.borrow()["items"].as_array().unwrap().len();
        context.stream_item(&thread_id, turn, json!({"id":format!("fixture-command-{suffix}"),"type":"commandExecution",
        "command":"./gradlew test","cwd":thread.borrow().metadata["cwd"],"aggregatedOutput":"running","status":"inProgress"}))?;

        match scenario {
        "retry" => context.notify("error", &json!({"threadId":thread_id,"turnId":turn_id,"willRetry":true,
            "error":{"message":"stream disconnected","additionalDetails":"attempt 2 of 5","codexErrorInfo":{"responseStreamDisconnected":{"httpStatusCode":429}}}}))?,
        "request" | "approval" => match self.request_outcome(&thread_id, &turn_id, scenario, &prompt).await? {
            RequestOutcome::Continue => {},
            RequestOutcome::Interrupted => return context.finish(thread, turn, "interrupted", None),
            RequestOutcome::Failed(error) => return context.finish(thread, turn, "failed", Some(error)),
        },
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
            ] { context.stream_item(&thread_id, turn, item)?; }
        }
        _ => (),
    }
        // Completion-only UI assertions do not need the artificial window used
        // to interact with running turns. Preserve that window for live scenarios.
        if !prompt.contains("[immediate]") && interrupted(stop, context.config.delay()).await {
            return context.finish(thread, turn, "interrupted", None);
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
        complete(context, &thread_id, turn, command_index)?;
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
                turn,
                json!({
                    "id":format!("fixture-edit-{suffix}"),"type":"fileChange","status":"completed",
                    "changes":[{"path":"tracked.txt","kind":{"type":"update"},
                        "diff":"-original\n+Edited during the turn\n"},
                        {"path":"added.txt","kind":{"type":"add"},
                        "diff":"+New first\n+New second\n+New third\n"}]
                }),
            )?;
            complete(context, &thread_id, turn, index)?;
            if !wait_for_release(&context.home, stop).await {
                return context.finish(thread, turn, "interrupted", None);
            }
        }
        if prompt.contains("[groups]") {
            context.stream_item(
                &thread_id,
                turn,
                json!({"id":format!("fixture-progress-{suffix}"),"type":"agentMessage",
            "text":"最初の確認が終わりました。次のコマンドを確認します。","phase":"commentary"}),
            )?;
            context.stream_item(&thread_id, turn, json!({"id":format!("fixture-next-command-{suffix}"),"type":"commandExecution",
            "command":"pwd","aggregatedOutput":"GROUP_DETAIL_OUTPUT","status":"completed","exitCode":0}))?;
            if !wait_for_release(&context.home, stop).await {
                return context.finish(thread, turn, "interrupted", None);
            }
        }
        if scenario == "failed" {
            let error =
                json!({"message":"context is full","codexErrorInfo":"contextWindowExceeded"});
            context.notify(
                "error",
                &json!({"threadId":thread_id,"turnId":turn_id,"willRetry":false,"error":error}),
            )?;
            return context.finish(thread, turn, "failed", Some(error));
        }
        if scenario == "interrupted" {
            return context.finish(thread, turn, "interrupted", None);
        }
        let response_text = self.prepare_response(&thread_id, &prompt, scenario, image.as_ref())?;
        let final_id = format!("fixture-final-{suffix}");
        let index = turn.borrow()["items"].as_array().unwrap().len();
        context.stream_item(
            &thread_id,
            turn,
            json!({"id":final_id,"type":"agentMessage","text":"","phase":"final_answer"}),
        )?;
        if prompt.contains("[long-markdown]") {
            let body = "Markdown stream fixture ".repeat(80) + "\n";
            let prose = (0..300)
            .map(|index| format!("Paragraph {index}. {}\n\n", "Long selectable prose retains its complete text while the reader edits a draft. ".repeat(4)))
            .collect::<String>();
            let ending = format!("\n```\n\n{prose}**MARKDOWN_STREAM_COMPLETE**");
            for chunk in std::iter::once("```text\n")
                .chain(std::iter::repeat_n(body.as_str(), 36))
                .chain(std::iter::once(ending.as_str()))
            {
                if interrupted(stop, Duration::from_millis(250)).await {
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
        complete(context, &thread_id, turn, index)?;
        context.finish(thread, turn, "completed", None)?;
        if scenario == "duplicate" {
            thread.borrow_mut().turns.push(Rc::new(RefCell::new(json!({"id":turn_id,"status":"completed","items":[
            {"id":"duplicate-history-old","type":"agentMessage","phase":"final_answer","text":"Older AI response must remain visible."}]}))));
        }
        Ok(())
    }

    async fn request_outcome(
        &self,
        thread_id: &str,
        turn_id: &str,
        scenario: &str,
        prompt: &str,
    ) -> Result<RequestOutcome> {
        let mut params = json!({"threadId":thread_id,"turnId":turn_id,"itemId":format!("fixture-command-{}", self.suffix)});
        let method = if scenario == "request" {
            params["questions"] = json!([{"id":"continue","header":"継続","question":"このまま続けますか？",
                    "options":[{"label":"続ける","description":"作業を進めます"},{"label":"見直す","description":"方針を見直します"}],"isOther":true,"isSecret":false}]);
            "item/tool/requestUserInput"
        } else {
            params["command"] = "echo fixture".into();
            params["cwd"] = self.thread.borrow().metadata["cwd"].clone();
            params["reason"] = "結合テストの承認確認".into();
            "item/commandExecution/requestApproval"
        };
        let (request_id, reply) = self.context.request(method, &params)?;
        let response = tokio::select! {
            _ = self.stop.cancelled() => Value::Null,
            result = reply => result.unwrap_or(Value::Null),
        };
        self.context.notify(
            "serverRequest/resolved",
            &json!({"threadId":thread_id,"requestId":request_id}),
        )?;
        Ok(request_result(
            prompt,
            scenario,
            &response,
            self.stop.is_cancelled(),
        ))
    }

    fn prepare_response(
        &self,
        thread_id: &str,
        prompt: &str,
        scenario: &str,
        image: Option<&(PathBuf, String)>,
    ) -> Result<String> {
        let content = response_content(&self.context.home, prompt, scenario, &self.suffix, image);
        if let Some(path) = &content.visualization {
            fs::write(
                path,
                include_str!("../../../agent-core/tests/fixtures/visualize/icon-options.html"),
            )?;
        }
        if let Some((path, url)) = content.generated_images {
            for (name, saved) in [("saved", Some(path)), ("inline", None)] {
                let index = self.turn.borrow()["items"].as_array().unwrap().len();
                self.context.stream_item(thread_id, &self.turn, json!({"id":format!("fixture-generated-{name}-{}", self.suffix),"type":"imageGeneration","status":"inProgress","result":"","savedPath":null}))?;
                {
                    let mut turn = self.turn.borrow_mut();
                    let item = &mut turn["items"][index];
                    item["status"] = "completed".into();
                    item["savedPath"] = serde_json::to_value(saved)?;
                    item["result"] = url.split_once(',').unwrap().1.into();
                }
                complete(&self.context, thread_id, &self.turn, index)?;
            }
        }
        Ok(content.text)
    }
}

fn prepare_scenario_image(home: &Path, prompt: &str) -> Result<Option<(PathBuf, String)>> {
    if !prompt.contains("[images]") && !prompt.contains("[generated-images]") {
        return Ok(None);
    }
    let path = home.join("fixture image.png");
    let bytes = include_bytes!(
        "../../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
    );
    fs::write(&path, bytes)?;
    Ok(Some((
        path,
        format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
    )))
}

struct ResponseContent<'a> {
    text: String,
    visualization: Option<PathBuf>,
    generated_images: Option<&'a (PathBuf, String)>,
}

fn response_content<'a>(
    home: &Path,
    prompt: &str,
    scenario: &str,
    suffix: &str,
    image: Option<&'a (PathBuf, String)>,
) -> ResponseContent<'a> {
    let generated_images = image.filter(|_| prompt.contains("[generated-images]"));
    let mut visualization = None;
    let mut response_text = if prompt.contains("[selection]") {
        "Needle Alpha Bravo.\n\nSecond paragraph stays unselected.".to_owned()
    } else if prompt.contains("[visualize]") {
        let path = home.join(format!("icon-options-{suffix}.html"));
        visualization = Some(path.clone());
        format!("visualize{}", json!({"path": path}))
    } else if prompt.contains("[markdown-table]") {
        include_str!("../../../agent-core/tests/fixtures/markdown/table.md").to_owned()
    } else if scenario == "history" {
        "ローカル relay 構成で Mac・iPhone アプリの実装と検証を完了しました。\n\n- **Mac アプリ**：会話、リモート操作、添付・保存・差分を確認。\n- iPhone Simulator：**10/10 成功、スキップ 0**。\n- SwiftUI の会話表示と入力欄を更新しました。\n\n変更したファイルは、下の差分から確認できます。".to_owned()
    } else {
        "シミュレータで完了しました。".to_owned()
    };
    if let Some((path, url)) = image {
        response_text = format!(
            "Hostの画像です。\n\n![Hostから読み込んだ画像](<{}>)\n\nインライン画像です。\n\n![インライン画像]({url})",
            path.display()
        );
        if generated_images.is_some() {
            response_text = format!(
                "生成画像を表示しました。\n\n[生成画像を開く](<{}>)\n\n[ファイルを開く](hello.txt:1)\n\n[Webサイトを開く](https://example.com)",
                path.display()
            );
            if home.join("fixture video.mp4").is_file() {
                for name in [
                    "video.mp4",
                    "audio.wav",
                    "animation.gif",
                    "vector.svg",
                    "document.pdf",
                    "archive.zip",
                    "large.txt",
                    "unknown.bin",
                ] {
                    response_text.push_str(&format!(
                        "\n\n[{name}](<{}>)",
                        home.join(format!("fixture {name}")).display()
                    ));
                }
            }
        }
    }
    ResponseContent {
        text: response_text,
        visualization,
        generated_images,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn question_answers_match_the_ui_choice_and_skip_contracts() {
        let choice = json!({"answers":{"continue":{"answers":["続ける"]}}});
        let skip = json!({"answers":{}});
        assert_eq!(
            request_result("[request] [choice]", "request", &choice, false),
            RequestOutcome::Continue
        );
        assert_eq!(
            request_result("[request] [skip]", "request", &skip, false),
            RequestOutcome::Continue
        );
        let failed = RequestOutcome::Failed(json!({"message":"Unexpected question answer"}));
        assert_eq!(
            request_result("[request] [choice]", "request", &skip, false),
            failed
        );
        assert_eq!(
            request_result("[request] [skip]", "request", &choice, false),
            failed
        );
        assert_eq!(
            request_result("[request]", "request", &skip, false),
            RequestOutcome::Continue
        );
        assert_eq!(
            request_result("[request] [choice]", "request", &choice, true),
            RequestOutcome::Interrupted
        );
        assert_eq!(
            request_result("", "approval", &json!({"decision":"decline"}), false),
            RequestOutcome::Interrupted
        );
        assert_eq!(
            request_result("", "approval", &json!({"decision":"accept"}), false),
            RequestOutcome::Continue
        );
    }
}
