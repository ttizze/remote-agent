use crate::supervision::Child;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, sync::watch};

pub(crate) struct Fixture {
    _process_table: tokio::sync::SemaphorePermit<'static>,
    _temporary: tempfile::TempDir,
    pub root: PathBuf,
    env: BTreeMap<OsString, OsString>,
}

impl Fixture {
    pub async fn new() -> Self {
        // These fixtures inspect the process-wide descriptor table. Serialize
        // owners so another fixture cannot fork with a cleanup flock held.
        static PROCESS_TABLE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let process_table = PROCESS_TABLE.acquire().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let mut env = std::env::vars_os()
            .filter(|(key, _)| {
                let key = key.to_string_lossy();
                !key.starts_with("CARGO_")
                    && !matches!(
                        key.as_ref(),
                        "RUSTFLAGS" | "RUSTC_WRAPPER" | "RUSTC_WORKSPACE_WRAPPER"
                    )
            })
            .collect::<BTreeMap<_, _>>();
        env.insert(
            "CARGO_HOME".into(),
            root.join("cargo-home").into_os_string(),
        );
        Self {
            _process_table: process_table,
            _temporary: temporary,
            root,
            env,
        }
    }

    pub async fn project(&self, name: &str) -> PathBuf {
        let root = self.root.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        let profile: toml::Value = toml::from_str(include_str!("../../../../Cargo.toml")).unwrap();
        fs::write(root.join("Cargo.toml"), format!("[package]\nname='cache-check'\nversion='0.1.0'\nedition='2024'\n[workspace]\n[profile.dev]\ndebug='{}'\n", profile["profile"]["dev"]["debug"].as_str().unwrap())).unwrap();
        fs::write(root.join("src/main.rs"), r#"
use std::{io::{Read, Write}, process::{Command, Stdio}, sync::atomic::{AtomicU32, Ordering}, time::Duration};
static SIGNAL: AtomicU32 = AtomicU32::new(0);
extern "C" fn record(signal: i32) { SIGNAL.store(signal as u32, Ordering::SeqCst); }
unsafe extern "C" { fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize; fn kill(pid: i32, signal: i32) -> i32; }
fn main() {
 let args: Vec<_> = std::env::args().collect();
 match args.get(1).map(String::as_str) {
  Some("-P") => {
   let port=args[args.iter().position(|arg| arg=="networkPort").unwrap()+1].parse::<u16>().unwrap();
   let mut response=String::new(); std::net::TcpStream::connect(("127.0.0.1", port)).unwrap().read_to_string(&mut response).unwrap();
   assert_eq!(response, "bex-os-network-ok\n"); print!("{response}");
   if args[2]!="missing" { println!("OK (1 test)"); }
   if args[2]=="failed" { std::process::exit(1); }
  }
  Some("hold") => { let mut line=String::new(); std::io::stdin().read_line(&mut line).unwrap(); }
  Some("panic") => fail(),
  Some("provider") => {
   unsafe { signal(15, record); } let root=std::path::Path::new(&args[2]);
   if args.get(3).map(String::as_str)==Some("--lifetime") { std::thread::spawn(|| { std::io::stdin().read_to_end(&mut Vec::new()).unwrap(); SIGNAL.store(15, Ordering::SeqCst); }); }
   std::fs::write(root.join("ready"), "").unwrap();
   while SIGNAL.load(Ordering::SeqCst)==0 { std::thread::sleep(Duration::from_millis(10)); }
   std::fs::write(root.join("stopped"), "").unwrap();
  }
  Some("parent") => { Command::new(&args[0]).args(["provider", &args[2]]).status().unwrap(); }
  Some("slow") | Some("controller") => {
   use std::os::unix::process::CommandExt;
   unsafe { signal(15, record); } let root=std::path::Path::new(&args[2]);
   let mut provider=if args[1]=="controller" { Some(Command::new(&args[0]).args(["provider", &args[2], "--lifetime"]).stdin(Stdio::piped()).process_group(0).spawn().unwrap()) } else { std::fs::write(root.join("ready"), "").unwrap(); None };
   while SIGNAL.load(Ordering::SeqCst)==0 { std::thread::sleep(Duration::from_millis(10)); }
   std::thread::sleep(Duration::from_millis(350));
   if let Some(provider)=provider.as_mut() { unsafe { kill(provider.id() as i32, 15); } assert!(provider.wait().unwrap().success()); }
   std::fs::write(root.join("cleaned"), "").unwrap();
  }
  Some("supervisor") => {
   unsafe { signal(2, record); } let root=std::path::Path::new(&args[2]); std::fs::write(root.join("ready"), "").unwrap();
   std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
   std::fs::write(root.join(if SIGNAL.load(Ordering::SeqCst)==2 { "interrupted" } else { "closed" }), "").unwrap();
  }
  Some("host") => {
   unsafe { signal(2, record); } let mut child=Command::new(&args[0]).args(["supervisor", &args[2]]).stdin(Stdio::piped()).spawn().unwrap();
   while SIGNAL.load(Ordering::SeqCst)==0 { std::thread::sleep(Duration::from_millis(10)); }
   drop(child.stdin.take()); assert!(child.wait().unwrap().success());
  }
  _ => { writeln!(std::io::stdout(), "cache-ready").unwrap(); }
 }
}
#[inline(never)] fn fail() { panic!("backtrace check"); }
"#).unwrap();
        self.build(&root, &root.join("target")).await;
        root
    }

    pub fn cargo(&self, root: &Path, target: &Path) -> Command {
        let mut command = Command::new("cargo");
        command
            .args(["build", "--offline", "--target-dir"])
            .arg(target)
            .current_dir(root)
            .env_clear()
            .envs(&self.env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command
    }

    pub async fn build(&self, root: &Path, target: &Path) {
        let (_sender, cancel) = watch::channel(false);
        let output = Child::spawn(self.cargo(root, target))
            .unwrap()
            .output(&cancel, Duration::from_secs(60), Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn executable(root: &Path) -> PathBuf {
        root.join("target/debug/cache-check")
    }

    pub fn runs(root: &Path) {
        let output = std::process::Command::new(Self::executable(root))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"cache-ready\n");
    }

    pub fn age(path: &Path, days: u64) {
        let timestamp = filetime::FileTime::from_system_time(
            std::time::SystemTime::now() - Duration::from_secs(days * 86400),
        );
        let mut pending = vec![path.to_owned()];
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.extend(
                    fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            }
            filetime::set_symlink_file_times(path, timestamp, timestamp).unwrap();
        }
    }
}
