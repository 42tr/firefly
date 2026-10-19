# Firefly LLM

模型调用与流式响应解析、工具参数校验与重新生成、上下文预算、模型限制发现及每次运行的上下文压缩。

通过 `AgentModel::new(client, api_key, model)` 传入可复用的 `reqwest::Client`；生成请求与模型元数据查询使用同一客户端。可选调用 `with_limits_cache(pool)` 持久化成功查询到的模型限制，连接池和数据库迁移由主程序管理。提供方、模型和凭据共同决定缓存键，调用者的输入上限不写入缓存。

`AgentModel::budget()` 返回对应的 `ModelBudget`。其他模型可使用 `ModelBudget::fixed(cap)` 指定预算，再通过 `ContextModel::with_budget()` 包装。`ContextModel::prepare()` 可单独准备和压缩请求，返回压缩过程产生的用量，供分支快照等业务使用。

```sh
cargo test --locked -p firefly-llm
```

回归使用本地模拟模型服务和临时 SQLite 数据库，验证流式输出、工具参数修复、预算约束、共享客户端、持久缓存及服务中断后的恢复。需要真实模型凭据的元数据检查默认忽略，只有显式设置 `FIREFLY_MODEL_CONTEXT_CHECK_ENV` 后才手动运行。
