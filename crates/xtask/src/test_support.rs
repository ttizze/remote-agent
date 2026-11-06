use crate::supervision::Child;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, sync::watch};

pub(crate) struct Fixture(tempfile::TempDir);

impl Fixture {
    pub fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    pub async fn project(&self, name: &str) -> PathBuf {
        let root = self.0.path().join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), r#"
use std::{io::Read, process::{Command, Stdio}, sync::atomic::{AtomicU32, Ordering}, time::Duration};
static SIGNAL: AtomicU32 = AtomicU32::new(0);
extern "C" fn record(signal: i32) { SIGNAL.store(signal as u32, Ordering::SeqCst); }
unsafe extern "C" { fn signal(signal: i32, handler: extern "C" fn(i32)) -> usize; fn kill(pid: i32, signal: i32) -> i32; }
fn main() {
 let args: Vec<_> = std::env::args().collect();
 match args.get(1).map(String::as_str) {
  Some("-P") => {
   let port=args[args.iter().position(|arg| arg=="networkPort").unwrap()+1].parse::<u16>().unwrap();
   let mut response=String::new(); std::net::TcpStream::connect(("127.0.0.1", port)).unwrap().read_to_string(&mut response).unwrap();
   assert_eq!(response, "agent-os-network-ok\n"); print!("{response}");
   if args[2]!="missing" { println!("OK (1 test)"); }
   if args[2]=="failed" { std::process::exit(1); }
  }
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
  _ => panic!("unexpected fixture mode"),
 }
}
"#).unwrap();
        let mut command = Command::new("rustc");
        command
            .arg("--edition=2024")
            .arg("src/main.rs")
            .arg("-o")
            .arg(Self::executable(&root))
            .current_dir(&root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (_sender, cancel) = watch::channel(false);
        let output = Child::spawn(command)
            .unwrap()
            .output(&cancel, Duration::from_secs(60), Duration::from_secs(10))
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        root
    }

    pub fn executable(root: &Path) -> PathBuf {
        root.join("fixture")
    }
}
