# FileBack

[![Platform](https://img.shields.io/badge/platform-macOS-black)](#系统要求)
[![Rust](https://img.shields.io/badge/rust-2024-orange)](#系统要求)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

FileBack 是一款使用 Rust 与 GPUI 构建的 macOS 文件恢复工具：先以只读方式扫描镜像或物理磁盘，再由用户勾选候选文件，最后经二次确认恢复到另一块磁盘上的独立目录。

> **项目状态：早期开发。** FileBack 使用文件签名雕刻，并可对 GPT 磁盘中的 APFS 容器做有限的只读来源分析；这不是完整的 APFS 文件系统恢复，也不能保证恢复所有文件。不要将本工具作为唯一的数据恢复手段，更不要恢复到源磁盘。

## 功能

- 扫描普通磁盘镜像或 macOS 物理磁盘。
- 识别 JPEG、PNG、PDF、ZIP、DOCX、XLSX、PPTX、MP4、MOV、M4A、AVI 和 WAV，覆盖跨读取块的文件头。
- 按图片、文档、表格、演示、视频、音频和压缩包分类显示，并恢复到对应子目录。
- 当前页按需生成 JPEG、PNG 缩略图和图片尺寸，尽量读取 DOCX、XLSX、PPTX 的内部标题及时间；预览失败不影响选择和恢复。
- 物理盘扫描阶段只记录候选偏移，不写入任何恢复文件；预览也只读取候选范围，不创建恢复文件。
- 物理盘扫描后独立执行只读 APFS 来源分析，显示现存文件引用、快照保留、疑似已删除或无法判断；失败不会丢失候选或阻断恢复。
- 实时显示扫描进度和候选结果，支持在读取及雕刻阶段暂停与取消。
- 每次扫描使用随机的私有会话目录，避免多实例进度和控制指令互相干扰。
- 扫描报告绑定源设备路径、容量与设备指纹；设备变化或旧报告会被拒绝。
- 候选默认不勾选，支持分页、本页选择和全局已选数量显示。
- 图形界面可将另一块磁盘上的任意已有目录选作恢复位置，确认后自动新建独立子目录。
- 恢复前校验目标目录为空，并检查目标不位于源物理盘。
- 恢复文件先写入临时文件，完整同步后再发布，不覆盖已有文件。
- 提供可纵向滚动的 GPUI 图形界面与独立命令行工具。
- 显示 APFS 本地快照和 Time Machine 配置状态。

## 支持格式与分类

| 分类 | 格式 | 输出目录 |
| --- | --- | --- |
| 图片 | JPEG、PNG | `images/` |
| 文档 | PDF、Word（DOCX） | `documents/` |
| 表格 | Excel（XLSX） | `spreadsheets/` |
| 演示 | PowerPoint（PPTX） | `presentations/` |
| 视频 | MP4、QuickTime（MOV）、AVI | `videos/` |
| 音频 | M4A、WAV | `audio/` |
| 压缩包 | ZIP，以及无法可靠细分的 ZIP 容器 | `archives/` |

DOCX、XLSX 和 PPTX 通过 ZIP 中央目录中的内容类型与唯一主文档路径识别；无法验证或属于 ZIP64 的容器保守保留为 ZIP。JPEG 会验证图像帧头与扫描数据头，过滤明显无效的签名匹配，但不要求完整解码。ISO 媒体按品牌区分 MP4、MOV 和 M4A，并仅在找到可验证的 `mdat` 与 `moov` 边界时生成候选；RIFF 按声明长度确定 AVI 和 WAV 边界。默认单文件上限为 4 GiB。

当前不支持旧版 DOC、XLS、PPT，也不保证支持 fragmented MP4、延伸到文件末尾的 `size = 0` atom 或任意视频编码。签名识别不做完整解码、CRC 或解压校验；碎片化文件即使签名匹配也可能损坏，恢复后必须实际打开验证。

候选预览仅针对当前页最多 10 个项目，通过受限磁盘助手只读完成。JPEG、PNG 可显示缩略图和原始尺寸，DOCX、XLSX、PPTX 可显示内部标题和时间；源文件超过 64 MiB 时不读取预览，但仍可正常恢复。PDF、视频、音频和普通 ZIP 目前显示类型占位，不生成内容缩略图。签名雕刻通常无法恢复原文件名，自动编号与元数据标题都不应被当作原始路径的证明。

## 工作原理

1. **扫描**：以只读方式打开源镜像或 `/dev/rdiskN`，根据文件头、容器结构或结束标记定位候选。
2. **来源分析**：物理盘扫描完成后，独立比对 GPT／APFS 当前文件、快照和空闲块；失败只显示说明，不移除候选。
3. **确认**：用户分页检查候选范围并明确选择需要恢复的项目，界面不会默认全选。
4. **校验**：恢复前再次核对扫描报告与当前物理盘的身份，拒绝过期或来源不匹配的报告。
5. **恢复**：选择另一块磁盘上的位置后自动规划独立子目录，用户确认时才创建并写入；文件按类别存入子目录，只有完整写入的文件才使用最终文件名。
6. **验证**：用户必须使用对应应用程序打开恢复文件并验证完整性。

### APFS 来源判断

| 状态 | 判断依据 |
| --- | --- |
| 现存文件引用 | 候选范围被当前卷中文件的物理 extent 完整覆盖。 |
| 快照中可恢复 | 当前文件未完整引用该范围，但计划内 APFS 快照完整覆盖它。 |
| 疑似已删除 | 候选范围完全位于当前空闲块；这是删除后的常见迹象，**不是确定删除的证明**，也不保证内容完整。 |
| 无法判断 | 证据不足、仅部分覆盖、元数据损坏、超出资源上限或格式不受支持。 |

仅当候选与唯一的、逻辑和物理均连续的主数据流精确对应时，才显示卷宗、原路径或快照名；多个路径或碎片化匹配不会被猜测为原文件名。分析限制包括每个容器 100 万个目录条目、128 层目录、128 个快照（每卷最多 32 个）及每个文件 100 万个 extent，达到上限会显示警告并保守保留“无法判断”。

当前只支持 GPT 磁盘上的 APFS 只读分析，不尝试解密 FileVault；Fusion、多卷快照及真实设备的兼容性仍需专项验证。SSD、APFS 快照和 TRIM 会影响数据是否仍然存在。基于签名的恢复无法可靠重建所有原文件名、目录结构或文件格式。

## 系统要求

- macOS 13 或更高版本（物理磁盘扫描与恢复目前仅支持 macOS）。
- `rust-toolchain.toml` 指定的 Rust 工具链。
- 默认图形界面构建需要 Xcode 命令行工具和 Metal Toolchain。
- 首次物理磁盘操作需要管理员确认，以安装受限的 launchd 按需助手。
- 原始设备访问需要为 `FileBack.app` 和安装后的 `dev.fileback.FileBack.helper` 开启“完全磁盘访问权限”；外置盘还需允许主程序弹出的“可移除卷宗”请求。不要再单独授权应用包内的 `fileback-cli`。

如果构建 GPUI 时提示缺少 Metal Toolchain，请按 Xcode 提示安装：

```bash
xcodebuild -downloadComponent MetalToolchain
```

## 构建与运行

### macOS 应用

```bash
./scripts/package-macos.sh
open dist/FileBack.app
```

打包脚本会同时放入 GUI、`fileback-cli` 助手和 launchd 配置，并使用固定 Bundle ID 进行本地 ad-hoc 签名。首次点击物理盘扫描时，FileBack 会请求管理员确认以安装受限助手；助手仅在收到请求时运行，并校验 FileBack 的签名。

在“系统设置 → 隐私与安全性 → 完全磁盘访问权限”中允许 **FileBack.app** 和 **dev.fileback.FileBack.helper**，然后重新打开应用。实际读取原始磁盘的是已安装助手，只授权主程序并不足够。管理员密码不能替代隐私授权，也不需要单独添加应用包内的 `fileback-cli`。重新构建会改变本地 ad-hoc 签名，若授权失效需要重新确认；正式发布使用 Developer ID 可避免这类重复授权。

开发机暂时没有 Metal Toolchain 时，可使用运行时着色器模式进行本地开发和权限验证：

```bash
FILEBACK_RUNTIME_SHADERS=1 ./scripts/package-macos.sh
```

正式发布应使用默认构建，并替换为 Developer ID 签名与公证；ad-hoc 签名仅适合本机开发。

### 命令行

```bash
# 查看系统备份状态
cargo run --no-default-features --bin fileback-cli -- status

# 列出检测到的物理磁盘
cargo run --no-default-features --bin fileback-cli -- disks

# 扫描镜像并恢复到另一设备上的空目录
cargo run --no-default-features --bin fileback-cli -- \
  scan "/path/to/image.raw" "/path/to/another-disk/empty-directory"
```

直接操作物理磁盘时，建议使用图形界面完成授权和二次确认。图形界面会自动执行来源分析；命令行也提供 `scan-device`、`analyze-device <diskN> <报告JSON> [--json]` 与 `recover-device`，运行不带参数的命令即可查看完整语法。

## 项目结构

```text
src/
├── app/                 # GPUI 界面、动作与交互状态
├── device/              # 物理盘只读扫描、候选合并与范围恢复
│   ├── mod.rs
│   └── tests.rs
├── filesystem/          # GPT 校验、APFS 来源证据与保守分类
│   ├── mod.rs
│   ├── gpt.rs
│   ├── apfs.rs
│   └── tests.rs
├── recovery/            # 恢复领域模型、格式识别与镜像扫描
│   ├── mod.rs
│   ├── model.rs         # 文件类型、分类与扫描报告
│   ├── format.rs        # 签名、ZIP 分类与媒体容器边界
│   ├── preview.rs       # 受限缩略图与 Office 元数据
│   └── tests.rs
├── system/              # macOS 系统适配
│   ├── disk.rs          # 磁盘发现与目标盘校验
│   ├── helper.rs        # 按需助手、客户端签名与 IPC 校验
│   ├── privilege.rs     # 管理员授权与进程间协作
│   ├── status.rs        # 快照与 Time Machine 状态
│   └── tests.rs
├── cli.rs               # CLI 入口
├── lib.rs               # 库入口
└── main.rs              # GUI 入口
```

详见 [架构说明](docs/ARCHITECTURE.md)。

## 质量检查

```bash
cargo fmt --all -- --check
cargo clippy --no-default-features --all-targets -- -D warnings
cargo test --no-default-features

# 需要完整 Metal 构建环境
cargo check --all-features
```

核心测试使用临时目录和合成镜像，不需要管理员权限，也不应访问真实磁盘。

## 参与贡献

- [贡献指南](CONTRIBUTING.md)
- [行为准则](CODE_OF_CONDUCT.md)
- [安全政策](SECURITY.md)
- [变更记录](CHANGELOG.md)

提交前请运行质量检查；涉及文件边界、权限或目标盘校验的修改必须补充对应测试。

## 许可证

本项目采用 [Apache License 2.0](LICENSE)。贡献者提交的代码默认按同一许可证授权。

## 致谢

- [GPUI](https://gpui.rs)：图形界面框架。
- [Zed](https://github.com/zed-industries/zed)：模块划分、贡献规范与质量流程的参考对象。

FileBack 是独立项目，与 Zed Industries 无隶属或背书关系。
