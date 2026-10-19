# Firefly Workspace

原生工作区准备、文件描述符访问、目录列表、图片校验和 ZIP 下载。此库不依赖应用配置、数据库或 HTTP 服务。

主程序将 `--workspace-worker` 参数交给 `prepare::run()`，将 `--file-worker` 参数交给 `files::run()`；库函数通过 stdout 输出既有协议响应，仍由同一个 Firefly 二进制启动子进程。文件操作使用 Unix API。

```sh
cargo test --locked -p firefly-workspace
```

调用实际二进制的回归测试见根目录 [贡献指南](../../CONTRIBUTING.md)。
