use serde_json::{json, Value};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Module {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    sequence: u64,
}

impl Module {
    fn start(directory: &Path, step: i64) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_firefly-sdk-counter-example"))
            .current_dir(directory)
            .env_clear()
            .env("FIREFLY_MODULE_DATA_DIR", directory.join("data"))
            .env(
                "FIREFLY_MODULE_CONFIG",
                json!({"label":"测试计数器","step":step}).to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
            sequence: 0,
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.sequence += 1;
        let message = json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params});
        let input = self.input.as_mut().unwrap();
        input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        let mut line = String::new();
        let size = tokio::time::timeout(Duration::from_secs(10), self.output.read_line(&mut line))
            .await
            .expect("module response timed out")
            .unwrap();
        assert!(size > 0, "module exited before replying");
        let reply: Value = serde_json::from_str(&line).expect("stdout must contain MCP JSON only");
        assert_eq!(reply["id"], self.sequence);
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }

    async fn initialize(&mut self) {
        let result = self
            .call(
                "initialize",
                json!({"protocolVersion":"2025-06-18", "capabilities":{},
            "clientInfo":{"name":"example-test","version":"1"}}),
            )
            .await;
        assert_eq!(result["serverInfo"]["name"], "rust-sdk-counter");
    }

    async fn counter(&mut self, tool: &str, path: &str) -> Value {
        let mut arguments = json!({"path":path});
        if tool == "request" {
            arguments["method"] = json!("POST");
        }
        let result = self
            .call("tools/call", json!({"name":tool,"arguments":arguments}))
            .await;
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["status"], 200);
        result["structuredContent"]["body"].clone()
    }

    async fn stop(mut self) {
        drop(self.input.take());
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .expect("module did not exit after stdin closed")
            .unwrap();
        assert!(status.success());
    }
}

#[tokio::test]
async fn sdk_tools_use_configuration_and_preserve_data_across_process_restarts() {
    let directory = tempfile::Builder::new()
        .prefix("sdk example ")
        .tempdir()
        .unwrap();
    std::fs::write(
        directory.path().join("ui.html"),
        "<!doctype html><title>test page</title>",
    )
    .unwrap();
    let mut module = Module::start(directory.path(), 3);
    module.initialize().await;
    let tools = module.call("tools/list", json!({})).await;
    let open = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "open_app")
        .unwrap();
    assert_eq!(
        open["_meta"]["ui"]["resourceUri"],
        "ui://rust-sdk-counter/app"
    );
    let resource = module
        .call("resources/read", json!({"uri":"ui://rust-sdk-counter/app"}))
        .await;
    assert!(resource["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("test page"));
    assert_eq!(
        module.counter("read", "/counter").await,
        json!({"label":"测试计数器","step":3,"value":0})
    );
    assert_eq!(module.counter("request", "/increment").await["value"], 3);
    assert_eq!(module.counter("request", "/increment").await["value"], 6);
    // A read tool cannot invoke the POST route or change the persisted value.
    let read_write_route = module
        .call(
            "tools/call",
            json!({"name":"read","arguments":{"path":"/increment"}}),
        )
        .await;
    assert_eq!(read_write_route["structuredContent"]["status"], 405);
    assert_eq!(module.counter("read", "/counter").await["value"], 6);
    module.stop().await;

    let mut restarted = Module::start(directory.path(), 2);
    restarted.initialize().await;
    assert_eq!(restarted.counter("read", "/counter").await["value"], 6);
    assert_eq!(restarted.counter("request", "/increment").await["value"], 8);
    assert_eq!(restarted.counter("request", "/reset").await["value"], 0);
    restarted.stop().await;
    assert!(directory.path().join("data/counter.sqlite3").is_file());
}
