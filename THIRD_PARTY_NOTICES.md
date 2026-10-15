# 第三方声明

Firefly 的 MIT 许可证不改变第三方软件、图标及其衍生资源的许可证。二进制 Release 同时提供本文件和 [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)，后者包含锁文件对应的 Rust / npm 依赖清单与原始许可证、NOTICE 文本，也涵盖构建、测试及平台相关依赖。

## 随源码提供的图标

| 来源 | 许可证 | 保留的原始声明 |
| --- | --- | --- |
| [Feather](https://github.com/feathericons/feather) — Cole Bemis | MIT | `frontend/src/assets/icons/LICENSE`、`crates/module-sdk/ui/assets/icons/LICENSE` |
| [Lucide](https://github.com/lucide-icons/lucide) — Lucide contributors，部分衍生于 Feather | ISC / MIT | 两个图标目录中的 `LICENSE-lucide` |

完整图标许可文本也收入第三方许可证清单。新增或修改 SVG 时保留上述文件及对应归属。界面使用操作系统字体，不分发或下载 Google Fonts、IBM Plex Sans 或 Space Grotesk 字体文件。

## 主要运行依赖

Rust 使用 Axum / Tokio、SQLx / SQLite、Reqwest / Rustls、Glow、lopdf 等；前端使用 Vue、ECharts、Marked 和 DOMPurify。准确版本和每个直接、间接依赖的许可见完整清单，SQLite 的上游公共领域声明、加密库及其内含第三方代码的声明也随原始许可证文件保留。

Codex、Claude Code、GitHub CLI、模型服务与外部业务模块由用户另行安装或配置，不随 Firefly Release 分发，适用各自许可证及服务条款。外部模块作者负责自己的第三方声明。

## 更新声明

依赖变更后先执行 `npm ci`，再运行 `python3 scripts/generate_third_party_licenses.py` 并提交生成文件。脚本优先保留依赖包自带许可证和 NOTICE；少数未随注册表包提供许可文本的依赖使用 `scripts/license-texts/` 中来自 [SPDX License List](https://github.com/spdx/license-list-data) 的标准条款。发布流程检查清单是否与锁文件一致。
