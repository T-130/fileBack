# 项目协作约定

本文件适用于在项目内工作的编码代理和自动化工具。人类贡献者请同时阅读 `CONTRIBUTING.md`。

## 修改前

- 阅读 `README.md`、`docs/ARCHITECTURE.md` 和相关模块测试。
- 保持单 crate，不为形式上的“大型架构”引入 workspace。
- 结构重构不得夹带扫描算法、权限模型或界面行为变化。
- 保留现有公共模块路径，除非变更明确包含 API 迁移说明。

## 数据安全

- 不得在默认测试中访问真实物理盘或请求管理员权限。
- 测试只使用 `tempfile` 与合成镜像。
- 不读取、复制或上传用户真实恢复数据。
- 涉及路径、提权和外部命令的改动必须审查注入、符号链接与竞态风险。

## 完成后

至少运行：

```bash
cargo fmt --all -- --check
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test --no-default-features
```

GUI 相关改动还应运行 `cargo check --all-features`；环境缺依赖时如实报告，不得宣称验证通过。
