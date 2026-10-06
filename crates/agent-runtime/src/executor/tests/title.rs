//! ThreadTitleRegenerationService.test.ts (service cases).
use super::*;
use agent_domain::EffectBody;

async fn user_message(rig: &Rig, thread: &ThreadId, id: &str, text: &str) {
    let reply = rig
        .command(
            thread,
            Command::Send(message(id, text, DispatchMode::DeferStart)),
        )
        .await;
    assert!(matches!(reply, Reply::Run(_)), "{reply:?}");
}

async fn arm(rig: &Rig, thread: &ThreadId, command: &str) -> CommandId {
    let request = CommandId::new(command).unwrap();
    let reply = rig
        .registry
        .dispatch(
            thread,
            request.clone(),
            Command::RegenerateTitle,
            CommandOrigin::Client,
        )
        .await
        .unwrap()
        .reply;
    assert_eq!(reply, Reply::Accepted);
    request
}

async fn title(rig: &Rig, thread: &ThreadId) -> (String, Option<CommandId>) {
    let state = rig.state(thread).await;
    let thread = state.thread.as_ref().unwrap();
    (thread.title.clone(), thread.title_request.clone())
}

fn generations(rig: &Rig) -> Vec<TextGenerationRequest> {
    rig.ops.generations.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn arms_and_clears_the_regeneration_marker() {
    let rig = rig();
    let id = tid("thread:title:arm");
    rig.create(&id, None).await;
    let first = arm(&rig, &id, "command:title:arm:1").await;
    assert_eq!(title(&rig, &id).await.1, Some(first.clone()));
    let job = rig.job(&id, "GenerateTitle").await;
    assert_eq!(
        job.effect.body,
        EffectBody::GenerateTitle {
            request: first,
            message: None
        }
    );

    arm(&rig, &id, "command:title:arm:2").await;
    rig.command(
        &id,
        Command::Rename {
            title: "Manual title".into(),
        },
    )
    .await;
    assert_eq!(title(&rig, &id).await, ("Manual title".into(), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn skips_execution_when_the_marker_was_superseded_by_a_newer_request() {
    let rig = rig();
    let id = tid("thread:title:stale");
    rig.create(&id, None).await;
    let stale = arm(&rig, &id, "command:title:stale:1").await;
    let current = arm(&rig, &id, "command:title:stale:2").await;
    let mut job = rig.job(&id, "GenerateTitle").await;
    job.effect.body = EffectBody::GenerateTitle {
        request: stale,
        message: None,
    };

    assert_eq!(rig.execute(job).await, Ok(None));
    assert!(generations(&rig).is_empty());
    assert_eq!(title(&rig, &id).await, ("Seed title".into(), Some(current)));
}

#[tokio::test(flavor = "multi_thread")]
async fn lands_the_regenerated_title_from_the_conversation_digest() {
    let rig = rig();
    rig.ops.titles.lock().unwrap().push_back(Ok(
        r#"{"title":"Fresh title","needsRefinement":false}"#.into(),
    ));
    let id = tid("thread:title:landing");
    rig.create(&id, None).await;
    user_message(&rig, &id, "landing", "Investigate the flaky login test").await;
    arm(&rig, &id, "command:title:landing:1").await;
    rig.drain().await;

    let calls = generations(&rig);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cwd, "/repo");
    assert_eq!(calls[0].operation, "generateThreadTitle");
    assert!(
        calls[0]
            .prompt
            .contains("The previous title was \"Seed title\".")
    );
    assert!(
        calls[0]
            .prompt
            .contains("USER:\nInvestigate the flaky login test")
    );
    assert_eq!(title(&rig, &id).await, ("Fresh title".into(), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_current_title_for_the_fallback_a_repeat_or_a_failure() {
    for (case, generated) in [
        ("fallback", Ok(r#"{"title":"New thread"}"#.to_owned())),
        ("unchanged", Ok("Seed title".to_owned())),
        ("failure", Err("model unavailable".to_owned())),
    ] {
        let rig = rig();
        rig.ops.titles.lock().unwrap().push_back(generated);
        let id = tid(&format!("thread:title:{case}"));
        rig.create(&id, None).await;
        user_message(&rig, &id, case, "Some conversation").await;
        arm(&rig, &id, &format!("command:title:{case}:1")).await;
        rig.drain().await;

        assert_eq!(generations(&rig).len(), 1, "{case}");
        assert_eq!(
            title(&rig, &id).await,
            ("Seed title".into(), None),
            "{case}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn completes_without_generating_when_the_initial_message_is_unavailable() {
    let rig = rig();
    let id = tid("thread:title:missing");
    rig.create(&id, None).await;
    let request = arm(&rig, &id, "command:title:missing:1").await;
    let mut job = rig.job(&id, "GenerateTitle").await;
    job.effect.body = EffectBody::GenerateTitle {
        request: request.clone(),
        message: Some(MessageId::new("message:title:missing").unwrap()),
    };

    let result = rig.execute(job).await.unwrap().unwrap();
    assert_eq!(
        result,
        EffectResult::TitleGenerated {
            request,
            title: None
        }
    );
    rig.input(&id, Input::Effect(result)).await;
    assert!(generations(&rig).is_empty());
    assert_eq!(title(&rig, &id).await, ("Seed title".into(), None));
}

// "initial title retry: %s" (the interrupted case leaves the effect unsettled:
// the marker stays and a restart requeues it).
#[tokio::test(flavor = "multi_thread")]
async fn retries_an_initial_title_twice() {
    for outcome in ["success", "exhausted", "stale"] {
        let rig = rig();
        {
            let mut titles = rig.ops.titles.lock().unwrap();
            titles.push_back(Err("Temporary failure".into()));
            titles.push_back(Err("Temporary failure".into()));
            titles.push_back(if outcome == "success" {
                Ok(r#"{"title":"Recovered title"}"#.into())
            } else {
                Err("Temporary failure".into())
            });
        }
        let id = tid(&format!("thread:{outcome}"));
        rig.create(&id, None).await;
        let mut first = message("first", "Fix the title", DispatchMode::DeferStart);
        first.title_seed = Some("Seed title".into());
        rig.command(&id, Command::Send(first)).await;
        let request = title(&rig, &id).await.1.unwrap();
        rig.drain().await;
        assert_eq!(generations(&rig).len(), 1, "{outcome}");
        if outcome == "stale" {
            rig.command(
                &id,
                Command::Rename {
                    title: "Manual title".into(),
                },
            )
            .await;
        }
        for _ in 0..3 {
            rig.clock.advance(60_000);
            rig.drain().await;
        }

        assert_eq!(
            generations(&rig).len(),
            if outcome == "stale" { 1 } else { 3 },
            "{outcome}"
        );
        let (current, marker) = title(&rig, &id).await;
        assert_eq!(
            current,
            match outcome {
                "success" => "Recovered title",
                "stale" => "Manual title",
                _ => "Seed title",
            },
            "{outcome}"
        );
        assert_eq!(marker, None, "{outcome} {request}");
    }
}
