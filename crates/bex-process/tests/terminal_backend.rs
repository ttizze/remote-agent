#![cfg(unix)]
//! Real supervisor/Alacritty PTYs; the child only supplies controlled byte streams.
use alacritty_terminal::{
    Term,
    event::{VoidListener, WindowSize},
    index::{Column, Line, Point},
    term::Config,
    vte::ansi::Processor,
};
use bex_process::{PtyCommand, PtyEvent};
use std::{
    io::{Read, Write},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Fixture {
    child: Child,
    input: Option<ChildStdin>,
    output: Lines<BufReader<ChildStdout>>,
    directory: tempfile::TempDir,
}
impl Fixture {
    async fn start(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_bex-provider-supervisor"))
            .arg("--bex-pty")
            .env("BEX_TEST_TERMINAL_MODE", mode)
            .env(
                "BEX_TEST_TERMINAL_RELEASE",
                directory.path().join("release"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut fixture = Self {
            child,
            input: Some(input),
            output,
            directory,
        };
        fixture
            .send(PtyCommand::Start {
                command: vec![
                    std::env::current_exe()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    "--exact".into(),
                    "terminal_child".into(),
                    "--ignored".into(),
                    "--nocapture".into(),
                ],
                cwd: fixture.directory.path().to_string_lossy().into_owned(),
                rows: 24,
                cols: 80,
            })
            .await;
        assert!(matches!(fixture.event().await, PtyEvent::Started));
        fixture
    }
    async fn send(&mut self, command: PtyCommand) {
        let mut bytes = serde_json::to_vec(&command).unwrap();
        bytes.push(b'\n');
        self.input
            .as_mut()
            .unwrap()
            .write_all(&bytes)
            .await
            .unwrap();
    }
    async fn event(&mut self) -> PtyEvent {
        let line = tokio::time::timeout(Duration::from_secs(10), self.output.next_line())
            .await
            .expect("supervisor event deadline")
            .unwrap()
            .expect("supervisor output");
        let event: PtyEvent = serde_json::from_str(&line).unwrap();
        assert!(!matches!(event, PtyEvent::Failed { .. }), "{event:?}");
        event
    }
    async fn ack(&mut self, expected: u64) -> Option<String> {
        loop {
            if let PtyEvent::Ack { id, error } = self.event().await {
                assert_eq!(id, expected);
                return error;
            }
        }
    }
    async fn write(&mut self, id: u64, data: &[u8]) {
        self.send(PtyCommand::Write {
            id,
            data: data.to_vec(),
        })
        .await;
        assert_eq!(self.ack(id).await, None);
    }
    async fn output_until(&mut self, needle: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            if let PtyEvent::Output { data } = self.event().await {
                bytes.extend(data);
            }
            if bytes.windows(needle.len()).any(|part| part == needle) {
                return bytes;
            }
        }
    }
    async fn checkpoint(&mut self, id: u64, rows: u16, cols: u16) -> Vec<u8> {
        self.send(PtyCommand::Checkpoint { id, rows, cols }).await;
        loop {
            if let PtyEvent::Checkpoint {
                id: actual,
                data,
                rows: r,
                cols: c,
            } = self.event().await
            {
                assert_eq!((actual, r, c), (id, rows, cols));
                return data;
            }
        }
    }
    async fn finish(mut self) {
        drop(self.input.take());
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}
fn restore(bytes: &[u8]) -> (Term<VoidListener>, Processor) {
    let size = WindowSize {
        num_lines: 24,
        num_cols: 80,
        cell_width: 0,
        cell_height: 0,
    };
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let mut parser: Processor = Processor::default();
    parser.advance(&mut term, bytes);
    (term, parser)
}
fn screen_text(term: &Term<VoidListener>) -> String {
    (0..24)
        .flat_map(|line| {
            (0..80).map(move |column| term.grid()[Point::new(Line(line), Column(column))].c)
        })
        .collect()
}

#[tokio::test]
async fn supervisor_preserves_queries_checkpoints_resize_and_write_completion() {
    let mut fixture = Fixture::start("interactive").await;
    fixture.output_until(b"READY").await;
    // More than one command queue's worth of queries in one child write must not
    // consume one queue slot each or terminate an otherwise healthy terminal.
    fixture.write(1, b"p").await;
    fixture.output_until(b"QUERIES_OK").await;
    for (id, first, last, suffix) in [
        (2, b'u', b'v', b"Z".as_slice()),
        (6, b's', b't', b"\x1b[0m".as_slice()),
    ] {
        fixture.write(id, &[first]).await;
        fixture
            .output_until(if first == b'u' {
                b"\xf0\x9f"
            } else {
                b"\x1b[31"
            })
            .await;
        let (mut restored, mut parser) = restore(&fixture.checkpoint(id + 1, 24, 80).await);
        fixture.write(id + 2, &[last]).await;
        parser.advance(&mut restored, &fixture.output_until(suffix).await);
        let (current, _) = restore(&fixture.checkpoint(id + 3, 24, 80).await);
        for line in 0..24 {
            for column in 0..80 {
                let at = Point::new(Line(line), Column(column));
                assert_eq!(restored.grid()[at], current.grid()[at]);
            }
        }
        if first == b's' {
            assert!(screen_text(&restored).contains("RED"));
        }
    }
    fixture.write(10, b"y").await;
    fixture.output_until(b"SYNC").await;
    fixture.output_until(b"SYNC_DONE").await;
    assert!(screen_text(&restore(&fixture.checkpoint(11, 24, 80).await).0).contains("SYNC"));
    fixture
        .send(PtyCommand::Resize {
            id: 12,
            rows: 0,
            cols: 80,
        })
        .await;
    assert!(fixture.ack(12).await.is_some());
    fixture
        .send(PtyCommand::Resize {
            id: 13,
            rows: 40,
            cols: 100,
        })
        .await;
    assert_eq!(fixture.ack(13).await, None);
    fixture.checkpoint(14, 40, 100).await;
    fixture.write(15, b"r").await;
    fixture.output_until(b"SIZE:40:100").await;
    fixture.write(16, b"b").await;
    fixture.output_until(b"BLOCKED").await;
    fixture
        .send(PtyCommand::Write {
            id: 17,
            data: vec![b'x'; 128 * 1024],
        })
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(60), fixture.output.next_line())
            .await
            .is_err(),
        "write acknowledged before child read"
    );
    std::fs::write(fixture.directory.path().join("release"), b"go").unwrap();
    assert_eq!(fixture.ack(17).await, None);
    fixture.output_until(b"DRAINED").await;
    fixture.finish().await;
}

#[tokio::test]
async fn cancellation_unblocks_full_input_and_exit_preserves_large_output() {
    let mut fixture = Fixture::start("interactive").await;
    fixture.output_until(b"READY").await;
    fixture.write(1, b"b").await;
    fixture.output_until(b"BLOCKED").await;
    fixture
        .send(PtyCommand::Write {
            id: 2,
            data: vec![b'x'; 128 * 1024],
        })
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(60), fixture.output.next_line())
            .await
            .is_err()
    );
    fixture.finish().await;
    let mut fixture = Fixture::start("exit-with-input").await;
    fixture.output_until(b"READY").await;
    fixture.write(1, b"b").await;
    fixture.output_until(b"BLOCKED").await;
    fixture
        .send(PtyCommand::Write {
            id: 2,
            data: vec![b'x'; 128 * 1024],
        })
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(60), fixture.output.next_line())
            .await
            .is_err()
    );
    std::fs::write(fixture.directory.path().join("release"), b"go").unwrap();
    let mut tail = 0;
    let mut failed_input = false;
    loop {
        match fixture.event().await {
            PtyEvent::Ack { id: 2, error } => failed_input = error.is_some(),
            PtyEvent::Output { data } => tail += data.iter().filter(|&&byte| byte == b'A').count(),
            PtyEvent::Exited { code } => {
                assert_eq!(code, 0);
                break;
            }
            _ => {}
        }
    }
    assert!(
        failed_input,
        "the child did not read the entire pending input"
    );
    assert_eq!(
        tail,
        1024 * 1024,
        "input failure must not discard exit output"
    );
    fixture.finish().await;
    let mut fixture = Fixture::start("exit-tail").await;
    let mut count = 0;
    loop {
        match fixture.event().await {
            PtyEvent::Output { data } => count += data.iter().filter(|&&b| b == b'A').count(),
            PtyEvent::Exited { code } => {
                assert_eq!(code, 0);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(count, 1024 * 1024);
    fixture.finish().await;
}

#[test]
#[ignore = "controlled subprocess child; invoked by the supervisor tests"]
fn terminal_child() {
    let mode = std::env::var("BEX_TEST_TERMINAL_MODE").expect("subprocess mode");
    unsafe {
        let mut termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(0, &mut termios), 0);
        libc::cfmakeraw(&mut termios);
        assert_eq!(libc::tcsetattr(0, libc::TCSANOW, &termios), 0);
    }
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    if mode == "exit-tail" {
        output.write_all(&vec![b'A'; 1024 * 1024]).unwrap();
        output.flush().unwrap();
        return;
    }
    output.write_all(b"READY").unwrap();
    output.flush().unwrap();
    if mode == "exit-with-input" {
        input.read_exact(&mut [0]).unwrap();
        output.write_all(b"BLOCKED").unwrap();
        output.flush().unwrap();
        let release = std::env::var_os("BEX_TEST_TERMINAL_RELEASE").unwrap();
        while !std::path::Path::new(&release).exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        output.write_all(&vec![b'A'; 1024 * 1024]).unwrap();
        output.flush().unwrap();
        return;
    }
    loop {
        let mut byte = [0];
        if input.read_exact(&mut byte).is_err() {
            return;
        }
        match byte[0] {
            b'p' => {
                output.write_all(&b"\x1b[6n".repeat(64)).unwrap();
                output.flush().unwrap();
                let mut replies = 0;
                while replies < 64 {
                    input.read_exact(&mut byte).unwrap();
                    if byte[0] == b'R' {
                        replies += 1;
                    }
                }
                output.write_all(b"QUERIES_OK").unwrap();
            }
            b'u' => output.write_all(b"\xf0\x9f").unwrap(),
            b'v' => output.write_all(b"\x98\x80Z").unwrap(),
            b's' => output.write_all(b"\x1b[31").unwrap(),
            b't' => output.write_all(b"mRED\x1b[0m").unwrap(),
            b'y' => {
                output.write_all(b"\x1b[?2026hSYNC\x1b[6n").unwrap();
                output.flush().unwrap();
                loop {
                    input.read_exact(&mut byte).unwrap();
                    if byte[0] == b'R' {
                        break;
                    }
                }
                output.write_all(b"SYNC_DONE").unwrap();
            }
            b'r' => {
                let mut size: libc::winsize = unsafe { std::mem::zeroed() };
                assert_eq!(unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) }, 0);
                write!(output, "SIZE:{}:{}", size.ws_row, size.ws_col).unwrap();
            }
            b'b' => {
                output.write_all(b"BLOCKED").unwrap();
                output.flush().unwrap();
                let release = std::env::var_os("BEX_TEST_TERMINAL_RELEASE").unwrap();
                while !std::path::Path::new(&release).exists() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                input.read_exact(&mut vec![0; 128 * 1024]).unwrap();
                output.write_all(b"DRAINED").unwrap();
            }
            _ => {}
        }
        output.flush().unwrap();
    }
}
