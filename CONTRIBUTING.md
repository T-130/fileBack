# 贡献指南

感谢你对 FileBack 的关注。本项目处理磁盘与恢复数据，正确性、安全边界和可复现测试优先于功能数量。

## 开始之前

- 功能改动或较大重构请先通过 Issue 讨论目标和方案。
- 一个 Pull Request 只解决一个主题，避免混入无关格式化或功能。
- 不提交无法复现、无法解释或未经验证的修改。
- 可以使用 AI 辅助开发，但提交者必须理解并自审全部改动。

参与项目即表示同意遵守 [行为准则](CODE_OF_CONDUCT.md)。

## 开发环境

1. 安装 Rustup，并使用仓库根目录的 `rust-toolchain.toml`。
2. 构建 GUI 时安装 Xcode 命令行工具及 Metal Toolchain。
3. 进入项目根目录后构建：

```bash
cargo build --bins
```

仅开发核心逻辑时，可以不启用 GPUI：

```bash
cargo build --no-default-features --bin fileback-cli
```

## 模块约定

- `app`：只处理界面、用户动作与交互状态，不新增底层 carving 细节。
- `device`：物理盘读取、扫描调度、候选合并和选定范围恢复。
- `recovery`：共享领域模型、文件签名与普通镜像扫描。
- `system`：macOS 磁盘、权限和系统状态适配。
- `cli.rs`：参数解析与终端输出，业务流程应放入库模块。

修改前先阅读 [架构说明](docs/ARCHITECTURE.md)，不要为了方便随意反转模块依赖。

## 代码规范

- 使用 Rust 2024 和仓库固定的 `rustfmt` 配置。
- 所有警告视为 CI 失败；不要使用 `allow` 掩盖新增警告。
- 生产代码避免 `unwrap`、`expect` 和 panic；错误使用 `anyhow::Result` 并补充用户可理解的上下文。
- 新增 `unsafe` 必须同时写明安全不变量，并优先考虑安全替代方案。
- 公共类型与函数保持清晰、稳定的命名和文档；无关重构中不要改变公共 API。
- 禁止提交 `dbg!`、未完成占位实现、真实用户数据或包含隐私的日志。

## 测试要求

- 修复缺陷必须添加能复现原问题的回归测试。
- 扫描逻辑至少考虑：读取块边界、64 MiB 分块边界、空输入、截断数据、伪造签名和超大候选。
- 文件写入测试使用 `tempfile` 和合成镜像。
- 不要在默认测试套件中访问真实物理磁盘、请求管理员权限或修改用户目录。
- GUI 改动需附截图或录屏，并描述实际手工验证步骤。

## 提交前检查

```bash
cargo fmt --all -- --check
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test --no-default-features
cargo check --all-features
```

如果本机缺少 Metal Toolchain，无法执行最后一项时，请在 Pull Request 中明确说明，不要声称已经通过。

## 提交与 Pull Request

提交信息建议使用 Conventional Commits：

```text
feat: add recoverable file preview
fix: detect headers crossing scan block boundaries
docs: clarify recovery destination requirements
test: cover truncated zip end records
refactor: separate disk discovery from privileges
```

Pull Request 应包含：

1. 问题与方案摘要。
2. 实际执行的命令和测试结果。
3. 对源盘只读、目标盘隔离和管理员权限的影响分析。
4. GUI 改动的截图或录屏。
5. 已完成的自审与测试清单。

## 许可证

贡献内容按 [Apache License 2.0](LICENSE) 授权。不要复制来源或许可证不明确的代码；引入第三方代码时必须保留其版权和许可证信息。
