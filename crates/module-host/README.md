# Firefly Module Host

外部 MCP 模块的清单校验、stdio / HTTP 连接、工具适配、页面资源、配置和任务调度。

主程序调用 `start(pool, Config { directory, data_directory, timezone })` 启动全局宿主，通过 `shutdown()` 关闭；也可以调用 `Host::new(pool, config)` 创建独立实例。时区通过 `FIREFLY_MODULE_TIMEZONE` 传给子进程，各模块定时任务使用清单中的时区。

数据库连接池与迁移由应用管理。测试使用独立数据库和根目录的真实迁移，从仓库内安装协议示例，需要 Python 3：

```sh
cargo test --locked -p firefly-module-host
```

安装与开发协议见 [模块文档](../../docs/modules.md)。
