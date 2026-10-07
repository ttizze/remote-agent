import { spawn } from "node:child_process";
import { Transform } from "node:stream";
import { StringDecoder } from "node:string_decoder";

// SDK 0.3.293 hides host-only session state from its public message iterator.
// Expose that documented SDKMessage through the SDK's custom spawn hook so
// the Host can keep a persistent query open across queued and background work.
export function sessionStateStream() {
  const decoder = new StringDecoder("utf8");
  let buffered = "";
  function line(text) {
    if (text.includes('"session_state_changed"')) {
      const message = JSON.parse(text);
      if (message.type === "system" && message.subtype === "session_state_changed") {
        delete message.sdk_host_only;
        return JSON.stringify(message);
      }
    }
    return text;
  }
  return new Transform({
    transform(chunk, _, done) {
      try {
        buffered += decoder.write(chunk);
        let end;
        while ((end = buffered.indexOf("\n")) !== -1) {
          this.push(`${line(buffered.slice(0, end))}\n`);
          buffered = buffered.slice(end + 1);
        }
        done();
      } catch (error) { done(error); }
    },
    flush(done) {
      try {
        buffered += decoder.end();
        if (buffered) this.push(`${line(buffered)}\n`);
        done();
      } catch (error) { done(error); }
    },
  });
}

export function spawnClaudeCodeProcess({ command, args, ...options }) {
  const child = spawn(command, args, { ...options, stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
  const stdout = child.stdout;
  child.stdout = stdout.pipe(sessionStateStream());
  stdout.on("error", (error) => child.stdout.destroy(error));
  child.stderr.pipe(process.stderr, { end: false });
  return child;
}
