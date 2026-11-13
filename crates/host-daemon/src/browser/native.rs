use super::{credentials, mcp};
use agent_protocol::{
    browser::{BrowserAction, BrowserFrame, BrowserRequest, validate_displayed_page},
    browser_native::{Capture, DisplayedPage, NativeAction, NativeRequest},
};
use std::{path::PathBuf, sync::Arc};

pub struct Browser {
    state_directory: PathBuf,
    pub(super) stop: tokio_util::sync::CancellationToken,
    pub(super) bridge_directory: tempfile::TempDir,
}

impl Browser {
    pub async fn start(profile: PathBuf) -> Result<Arc<Self>, String> {
        let state_directory = profile
            .parent()
            .ok_or("ブラウザの保存先を取得できません。")?
            .to_owned();
        let bridge_directory = tempfile::Builder::new()
            .prefix("agent-browser-")
            .tempdir_in("/tmp")
            .map_err(|e| e.to_string())?;
        agent_local_host::create_state_directory(bridge_directory.path())
            .map_err(|e| e.to_string())?;
        let browser = Arc::new(Self {
            state_directory,
            stop: Default::default(),
            bridge_directory,
        });
        mcp::listen(&browser)?;
        Ok(browser)
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
    }

    async fn send(&self, request: &NativeRequest) -> Result<BrowserFrame, String> {
        request.validate()?;
        let directory = self.state_directory.clone();
        let renderer = tokio::task::spawn_blocking(move || {
            agent_local_host::browser::read_renderer(&directory)
        })
        .await
        .map_err(|_| "内蔵ブラウザの接続先を取得できません。")?
        .map_err(|_| "PC側のBexでウィンドウを開いてから、ブラウザを操作してください。")?;
        let frame = agent_transport::native_browser::request(&renderer.socket, request).await?;
        if frame.renderer_id != renderer.id {
            return Err("内蔵ブラウザが切り替わりました。画面を更新してください。".into());
        }
        Ok(frame)
    }

    pub async fn request(&self, request: &BrowserRequest) -> Result<BrowserFrame, String> {
        request.validate()?;
        let mut command = NativeRequest {
            scope: request.thread_id.to_string(),
            displayed: Some(DisplayedPage {
                tab_id: request.tab_id.clone(),
                generation: request.generation,
            }),
            image_id: request.image_id.clone(),
            renderer_id: request.renderer_id.clone(),
            capture: if matches!(request.action, BrowserAction::Snapshot) {
                Capture::Snapshot
            } else {
                Capture::Viewer
            },
            action: NativeAction::Browser(request.action.clone()),
        };
        if !credentials::manages_credentials(&request.action) {
            return self.send(&command).await;
        }
        command.capture = Capture::None;
        command.action = NativeAction::Browser(BrowserAction::Read);
        let current = self.send(&command).await?;
        validate_displayed_page(
            &current.tab_id,
            current.generation,
            &request.tab_id,
            request.generation,
            &request.action,
        )?;
        let origin = current
            .tabs
            .iter()
            .find(|t| t.id == current.tab_id)
            .and_then(|t| agent_protocol::browser_import::password_origin(&t.url).ok());
        if let BrowserAction::FillPassword {
            origin: expected, ..
        }
        | BrowserAction::DeletePasswords { origin: expected } = &request.action
            && origin.as_deref() != Some(expected.as_str())
        {
            return Err("表示中のログイン先が変わりました。もう一度一覧を開いてください。".into());
        }
        let prepared = credentials::prepare(request.action.clone(), origin).await?;
        let mut metadata = None;
        let notice = match prepared {
            credentials::Prepared::Metadata(value) => {
                metadata = Some(value);
                None
            }
            credentials::Prepared::Notice(notice) => Some(notice),
            credentials::Prepared::Cookies { cookies, skipped } => {
                let count = cookies.len();
                if count != 0 {
                    command.action = NativeAction::ImportCookies(cookies);
                }
                Some(agent_protocol::browser::BrowserNotice::CookiesImported {
                    count: count as u64,
                    skipped: skipped as u64,
                })
            }
            credentials::Prepared::Fill { origin, script } => {
                command.action = NativeAction::FillPassword { origin, script };
                Some(agent_protocol::browser::BrowserNotice::PasswordFilled)
            }
        };
        command.capture = Capture::Viewer;
        let mut frame = self.send(&command).await?;
        frame.credentials = metadata;
        frame.notice = notice;
        Ok(frame)
    }

    pub(super) async fn agent(
        &self,
        thread: &str,
        action: BrowserAction,
    ) -> Result<BrowserFrame, String> {
        super::validate_agent_action(&action)?;
        self.send(&NativeRequest {
            scope: thread.into(),
            displayed: None,
            image_id: String::new(),
            renderer_id: String::new(),
            capture: if matches!(action, BrowserAction::Snapshot) {
                Capture::Snapshot
            } else {
                Capture::None
            },
            action: NativeAction::Browser(action),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::session::{ProviderKind, SessionRef};
    #[tokio::test]
    async fn client_and_agent_alias_reach_the_same_native_conversation() {
        let root = tempfile::tempdir().unwrap();
        let lease = agent_local_host::browser::RendererLease::acquire(root.path()).unwrap();
        let listener = tokio::net::UnixListener::bind(&lease.renderer.socket).unwrap();
        lease.publish().unwrap();
        let id = lease.renderer.id.clone();
        let thread = SessionRef::new(ProviderKind::Codex, "same-tab".into()).unwrap();
        let scope = thread.to_string();
        let expected = scope.clone();
        let server = tokio::spawn(async move {
            use agent_transport::native_browser::{read, write};
            for client in [false, true] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: NativeRequest = read(&mut stream).await.unwrap();
                assert_eq!(request.scope, expected);
                assert_eq!(request.displayed.is_some(), client);
                assert!(matches!(
                    request.action,
                    NativeAction::Browser(BrowserAction::Read)
                ));
                write(
                    &mut stream,
                    &Ok::<_, String>(BrowserFrame {
                        tab_id: "shared".into(),
                        renderer_id: id.clone(),
                        ..Default::default()
                    }),
                )
                .await
                .unwrap();
            }
        });
        let browser = Browser::start(root.path().join("browser")).await.unwrap();
        assert_eq!(
            browser
                .agent(&scope, BrowserAction::Read)
                .await
                .unwrap()
                .tab_id,
            "shared"
        );
        assert_eq!(
            browser
                .request(&BrowserRequest {
                    thread_id: thread,
                    tab_id: String::new(),
                    image_id: String::new(),
                    generation: 0,
                    renderer_id: String::new(),
                    action: BrowserAction::Read
                })
                .await
                .unwrap()
                .tab_id,
            "shared"
        );
        server.await.unwrap();
        browser.shutdown().await;
    }
    #[tokio::test]
    async fn unavailable_renderer_requires_pc_and_agent_cannot_import_credentials() {
        let root = tempfile::tempdir().unwrap();
        let browser = Browser::start(root.path().join("browser")).await.unwrap();
        for action in [
            BrowserAction::Credentials,
            BrowserAction::ImportCookies {
                profile: "Chrome/Default".into(),
            },
            BrowserAction::ImportPasswords {
                profile: "Chrome/Default".into(),
            },
            BrowserAction::FillPassword {
                origin: "https://example.test".into(),
                username: "fixture".into(),
            },
            BrowserAction::DeletePasswords {
                origin: "https://example.test".into(),
            },
        ] {
            assert!(
                browser
                    .agent("scope", action)
                    .await
                    .unwrap_err()
                    .contains("ブラウザのメニュー")
            );
        }
        assert!(
            browser
                .agent("scope", BrowserAction::Read)
                .await
                .unwrap_err()
                .contains("PC側のBex")
        );
        browser.shutdown().await;
    }
}
