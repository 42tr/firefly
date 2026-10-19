use std::time::Duration;

use firefly_claude_transport::{session_id, text_delta, ClaudeTurn};
use serde_json::{json, Value};
use tokio::{process::Command, time::timeout};

fn python(script: &str) -> Command {
    let mut command = Command::new("python3");
    command.env_clear().args(["-u", "-c", script]);
    command
}

async fn next_event(turn: &mut ClaudeTurn) -> anyhow::Result<Option<Value>> {
    timeout(Duration::from_secs(5), turn.next_event())
        .await
        .expect("mock CLI stalled")
}

#[tokio::test]
async fn injected_command_preserves_args_environment_and_cwd_and_drains_stderr() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = python(
        "import json,os,sys\n\
         assert sys.stdin.read() == ''\n\
         sys.stderr.write('diagnostic\\n' * 8192)\n\
         sys.stderr.flush()\n\
         print('  \\n')\n\
         print(json.dumps({'type':'system','subtype':'init','session_id':'local-session',\
         'cwd':os.getcwd(),'argument':sys.argv[1],'value':os.environ['FIREFLY_TEST_VALUE']}))\n\
         print(json.dumps({'type':'stream_event','event':{'type':'content_block_delta',\
         'delta':{'type':'text_delta','text':'hello'}}}))",
    );
    command
        .current_dir(directory.path())
        .arg("--not-a-flag")
        .env("FIREFLY_TEST_VALUE", "local-value");
    let mut turn = ClaudeTurn::spawn(command).unwrap();
    let event = next_event(&mut turn).await.unwrap().unwrap();
    assert_eq!(session_id(&event), Some("local-session"));
    assert_eq!(
        event["cwd"],
        directory.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(event["argument"], "--not-a-flag");
    assert_eq!(event["value"], "local-value");
    assert_eq!(
        text_delta(&next_event(&mut turn).await.unwrap().unwrap()),
        Some("hello")
    );
    assert!(next_event(&mut turn).await.unwrap().is_none());
    turn.interrupt().await.unwrap();
    turn.interrupt().await.unwrap();
}

#[tokio::test]
async fn invalid_utf8_is_a_protocol_error() {
    let mut turn = ClaudeTurn::spawn(python(
        r"import sys;sys.stdout.buffer.write(b'\xff\n');sys.stdout.flush()",
    ))
    .unwrap();
    let error = next_event(&mut turn).await.unwrap_err();
    assert!(format!("{error:#}").contains("UTF-8"));
    turn.interrupt().await.unwrap();
}

#[tokio::test]
async fn malformed_json_is_a_protocol_error() {
    let mut turn = ClaudeTurn::spawn(python("print('not-json')")).unwrap();
    let error = next_event(&mut turn).await.unwrap_err();
    assert!(format!("{error:#}").contains("无法解析 Claude CLI 消息: not-json"));
    turn.interrupt().await.unwrap();
}

#[tokio::test]
async fn nonzero_exit_is_reported_after_the_last_event() {
    let mut turn = ClaudeTurn::spawn(python("import sys;print('{}');sys.exit(7)")).unwrap();
    assert_eq!(next_event(&mut turn).await.unwrap(), Some(json!({})));
    let error = next_event(&mut turn).await.unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("Claude CLI 异常退出"), "{message}");
    assert!(message.contains('7'), "{message}");
    turn.interrupt().await.unwrap();
}

#[cfg(target_os = "linux")]
fn descendant_command() -> Command {
    python("import json,subprocess,time\np=subprocess.Popen(['sleep','60'])\nprint(json.dumps({'pid':p.pid}),flush=True)\ntime.sleep(60)")
}

#[cfg(target_os = "linux")]
async fn assert_stopped(pid: u64) {
    timeout(Duration::from_secs(2), async {
        loop {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if state.as_ref().is_err()
                || state.unwrap().split(") ").nth(1).unwrap().starts_with('Z')
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("tool descendant survived its turn");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn interrupt_stops_local_tool_descendants() {
    let mut turn = ClaudeTurn::spawn(descendant_command()).unwrap();
    let pid = next_event(&mut turn).await.unwrap().unwrap()["pid"]
        .as_u64()
        .unwrap();
    turn.interrupt().await.unwrap();
    assert_stopped(pid).await;
    turn.interrupt().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_turn_stops_local_tool_descendants() {
    let mut turn = ClaudeTurn::spawn(descendant_command()).unwrap();
    let pid = next_event(&mut turn).await.unwrap().unwrap()["pid"]
        .as_u64()
        .unwrap();
    drop(turn);
    assert_stopped(pid).await;
}

#[test]
fn extracts_init_session_id() {
    let event = json!({ "type": "system", "subtype": "init", "session_id": "session-1" });
    assert_eq!(session_id(&event), Some("session-1"));
    assert_eq!(
        session_id(&json!({"type":"result", "session_id":"session-1"})),
        None
    );
}

#[test]
fn extracts_streamed_text_delta() {
    let event = json!({
        "type": "stream_event",
        "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "hello" } }
    });
    assert_eq!(text_delta(&event), Some("hello"));
    assert_eq!(
        text_delta(&json!({"type":"assistant", "text":"hello"})),
        None
    );
}
