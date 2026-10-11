# CAPL Playground

CAPL Playground 现在是一个 Rust-first、可本地运行的 CAPL 与 CAN 仿真工作台。编译器继续保持为可复用 Cargo workspace，桌面界面使用 Longbridge GPUI Kit，布局按 VS Code 风格组织，并支持可停靠面板。

核心路径是“真实记录 → 虚拟时间 → CAPL 事件处理器”：

```text
ASC / candump ─▶ trace model ─▶ 虚拟时间回放 ─▶ on message
DBC / LDF ────▶ database model ─▶ 信号解码 ─────▶ 表格和曲线
CAPL 源码 ────▶ 编译器 ─────────▶ WASM ──────────▶ 同一套 runtime
```

## Rust crates

```text
capl-parse          CAPL lexer 和 parser
capl-sema           语义分析
capl-driver         编译流程和 CLI
capl-codegen-*      C 与 WebAssembly 后端
capl-model          与格式无关的 CAN/LIN 模型
capl-trace-core     trace 事件和 reader 接口
capl-trace-asc      Vector ASC、candump 文本解析器
capl-database-core  报文、信号、节点和解码模型
capl-database-dbc   DBC 解析器
capl-project        .caplproj JSON 项目文件和存储抽象
capl-runtime        虚拟时钟、回放和原生 WASM 宿主
capl-sysvars        命名空间系统变量注册表和变更事件
capl-desktop        GPUI Kit 桌面应用
```

后续的 LDF、BLF、MF4、ARXML 都应新增对应 family crate，并输出统一模型；解析器不依赖 UI 或 runtime。

## 构建和测试

从 `rust/` 执行：

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p capl-desktop
cargo run -p capl-driver -- path/to/file.can --emit=wat
cargo run -p capl-replay -- path/to/node.can path/to/traffic.asc

# 浏览器/WASM GUI（PowerShell）
./rust/serve-web.ps1
```

本地桌面版使用 `cargo run -p capl-desktop`。浏览器版使用 Trunk；首次执行前安装：
`cargo install --locked trunk`。
GPUI 当前的 WebAssembly backend 需要 nightly，首次执行：
`rustup +nightly target add wasm32-unknown-unknown`；`serve-web.ps1` 会自动选择 nightly。

项目文件扩展名固定为 `.caplproj`。GPUI 原生版的 New/Open/Save Project 和 Open Trace
现在使用系统文件选择器；同一个 JSON manifest 也可以通过浏览器 local storage 适配器保存。

## 回放

`capl-trace-asc` 支持常见 Vector ASC 和 candump 文本行。runtime 使用虚拟时间，按 CAN id 和 channel 匹配报文，将帧写入 WASM 的 CAPL message 对象，然后调用匹配的 `on message` 导出函数。这样可以在接触真实车辆前，用实际交通记录检查 CAPL 脚本。

当前 DBC 解析器覆盖基础节点、报文和信号模型；二进制 trace 和完整 DBC/LDF 覆盖度继续以独立 crates 迭代。

## 许可证

MIT OR Apache-2.0。
