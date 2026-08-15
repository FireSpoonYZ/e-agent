# Pi Extension 与 Rust 工具的 PTC 兼容方案

## 1. 目标

在 Programmatic Tool Calling（PTC）中，将两类工具统一暴露为 ES module 的异步函数：

1. 通过 `e-agent-extension` ABI 加载的 Rust 工具。
2. 通过 Pi Extension API `pi.registerTool()` 注册的 TypeScript/JavaScript 工具。

PTC 程序不需要知道工具的实现语言：

```ts
import { list, update } from "todo";
import { greet } from "my_extension";

const items = await list({});
await update({ index: 0, status: "completed" });
const greeting = await greet({ name: "Alice" });

console.log(JSON.stringify({ items, greeting }));
```

统一发生在 PTC 的 `NativeModule` / `NativeFunction` 层，不修改官方 Pi Extension API，也不修改扩展源码。

## 2. 非目标

第一阶段不实现：

- 将 Pi Extension 工具直接暴露为顶层 LLM tools；LLM 仍只看到 `node`。
- `pi.tool()`、`callTool()` 等官方 Pi 不提供的工具间调用 API。
- commands、shortcuts、providers、自定义 UI 和渲染。
- session tree、持久化、reload 和 npm/git package 安装。
- 将 JS 工具闭包复制到 PTC runtime。

## 3. 当前实现

Rust 工具的现有调用链：

```text
Rust cdylib
  -> ProgrammaticToolExecutor::load()
  -> e_agent_extension_v1 ABI
  -> ToolExtension metadata
  -> NativeModule / NativeFunction
  -> PTC QuickJS import
  -> NativeCall
  -> call_extension()
```

PiJS runtime 已经具备：

- TypeScript 转译和 ES module 加载。
- `pi.registerTool()` 的 JS 注册表。
- `__pi_load_extension()`。
- `__pi_get_registered_tools()`。
- `__pi_execute_tool()`。
- Promise、timer、console 和 hostcall 基础设施。

缺口是将这些内部能力封装为持久的宿主 API，并把注册结果适配到现有 PTC module 系统。

## 4. 兼容契约

### 4.1 模块和函数

每个扩展对应一个 PTC module，每个工具对应一个导出函数：

| 来源 | 扩展标识 | 工具名 | PTC 调用 |
|---|---|---|---|
| Rust ABI | `todo` | `update` | `import { update } from "todo"` |
| Pi Extension | `my_extension` | `greet` | `import { greet } from "my_extension"` |

模块名必须满足：

```text
^[A-Za-z_$][A-Za-z0-9_$-]*$
```

函数名必须是合法的 JavaScript identifier。第一阶段遇到非法名称时加载失败并给出明确错误，不做静默改名。

模块名冲突和同一模块内的函数名冲突均在加载阶段报错。加载顺序不得决定覆盖结果。

### 4.2 参数

新生成的 PTC 描述统一要求单个 JSON object 参数：

```ts
await update({ index: 0, status: "completed" });
await greet({ name: "Alice" });
```

这样 Pi `registerTool()` 的 JSON Schema 可以直接使用，不依赖 `properties` 的迭代顺序。

为了兼容现有 Rust PTC 程序，Rust 函数继续接受旧 positional 形式：

```ts
await update(0, "completed"); // 兼容
```

调用适配规则：

```text
单个 plain object 参数
  -> 原样作为工具 input

否则
  -> 按 NativeFunction.parameters 将 positional 参数组装为 object
```

Pi Extension 工具只在 PTC 描述中宣传 object 形式。第一阶段不为它承诺 positional 兼容。

### 4.3 返回值

Rust 工具继续返回其 `output_schema` 声明的业务 JSON。

Pi Extension 工具返回 `execute()` 的完整结果：

```json
{
  "content": [{ "type": "text", "text": "Hello Alice" }],
  "details": {}
}
```

PTC 描述必须明确每个函数的输出 schema。第一阶段不猜测或自动提取 `content[0].text`，避免丢失 `details`、图片或自定义内容。

### 4.4 错误

两类工具统一以 rejected Promise 表示调用失败：

```text
Rust ABI callback is_error=true
  -> reject Error

Pi Extension execute() throw/reject
  -> reject Error

无效 JSON、未知模块、未知函数、schema 验证失败、超时
  -> reject Error
```

错误消息包含 `module.function`，不包含扩展源码、环境变量或完整敏感输入。

## 5. 目标架构

```text
                         PTC QuickJS runtime
                                  |
                    import { fn } from "module"
                                  |
                         NativeModule proxy
                                  |
                          PtcModuleRegistry
                           /              \
                          /                \
              RustAbiFunction          JsExtensionFunction
                    |                           |
             call_extension()        JsExtensionRuntimeHandle
                                                |
                                  __pi_execute_tool(extension, tool)
                                                |
                                  registered execute() closure
```

建议增加最小 registry：

```rust
struct PtcModuleRegistry {
    modules: HashMap<String, PtcModule>,
}

struct PtcModule {
    name: String,
    functions: HashMap<String, PtcFunction>,
}

enum PtcFunction {
    RustAbi {
        extension: Arc<LoadedExtension>,
        metadata: ToolFunction,
    },
    JsExtension {
        runtime: JsExtensionRuntimeHandle,
        extension_id: String,
        metadata: ExtensionToolDef,
    },
}
```

它只负责：

- 注册模块和函数。
- 检测名称冲突。
- 生成 `Vec<NativeModule>`。
- 按 `(module, function)` 路由调用。

不在第一阶段引入新的通用 Tool trait 或顶层 Agent ToolRegistry。

## 6. Pi Extension Runtime

### 6.1 持久生命周期

Pi Extension 的 factory 和工具 `execute` 都是 JS 闭包，因此 runtime 必须跨 PTC 调用存活：

```text
ProgrammaticToolExecutor 创建
  -> ExtensionRuntime 启动
  -> 加载所有 Pi extensions
  -> 多次 PTC 调用复用同一 runtime
  -> Session close / executor drop 时 shutdown
```

每次调用新建 runtime 会破坏扩展内存状态和 module cache，不可采用。

### 6.2 Actor Handle

`PiJsRuntime` 内部包含 QuickJS 单线程状态，不应直接放进共享 `Arc`。增加一个最小 actor handle：

```rust
enum JsExtensionCommand {
    Load {
        spec: JsExtensionLoadSpec,
        reply: oneshot::Sender<Result<()>>,
    },
    ListTools {
        reply: oneshot::Sender<Result<Vec<ExtensionToolDef>>>,
    },
    ExecuteTool {
        extension_id: String,
        tool_name: String,
        tool_call_id: String,
        input: Value,
        ctx: Value,
        reply: oneshot::Sender<Result<Value>>,
    },
    Shutdown,
}
```

`JsExtensionRuntimeHandle` 只包含 channel sender，因此可安全进入 PTC 的异步 `NativeCall` closure。

### 6.3 扩展身份

当前 JS 注册记录已经保存 `extensionId`，但 `__pi_get_registered_tools()` 只返回工具 spec。需要返回来源信息：

```rust
struct ExtensionToolDef {
    extension_id: String,
    name: String,
    label: Option<String>,
    description: String,
    parameters: Value,
}
```

执行接口必须按 `(extension_id, tool_name)` 查找，不能只按全局工具名查找：

```js
__pi_execute_extension_tool(
  bridgeSecret,
  extensionId,
  toolName,
  toolCallId,
  input,
  ctx,
);
```

旧的 `__pi_execute_tool(toolName, ...)` 可以保留给现有测试和兼容路径。

## 7. 加载流程

### 7.1 Rust ABI

保留现有 `ProgrammaticToolExecutor::load()` 行为，但加载成功后将函数注册到 `PtcModuleRegistry`：

```text
ToolExtension.name -> module name
ToolFunction.name  -> function name
ToolFunction.schema -> input schema
ToolFunction.output_schema -> output schema
```

### 7.2 Pi Extension

增加异步加载入口：

```rust
pub async fn load_pi_extension(&mut self, path: impl AsRef<Path>) -> Result<()>;
```

步骤：

1. canonicalize entry path。
2. 生成稳定 `extension_id` 和 module name。
3. 将扩展父目录注册为 module root。
4. 在持久 runtime 中调用 `__pi_load_extension()`。
5. 等待同步或异步 factory 完成。
6. 查询该 extension 注册的工具。
7. 校验名称和 JSON Schema。
8. 将工具注册为 `PtcFunction::JsExtension`。

第一阶段入口来源使用：

```text
E_AGENT_EXTENSION_PATHS
```

值按平台 path-list 规则解析，与 `E_AGENT_TOOL_PATHS` 一致。自动扫描 `~/.pi` 和 `.pi/extensions` 后置。

## 8. PTC 调用流程

`ProgrammaticToolExecutor::execute()` 不再直接从 `self.extensions` 构建 modules，而是使用 registry：

```rust
let modules = self.registry.native_modules();
let registry = Arc::clone(&self.registry);

let call: NativeCall = Arc::new(move |module, function, input| {
    let registry = Arc::clone(&registry);
    Box::pin(async move {
        registry.call(session, &module, &function, input).await
    })
});
```

Rust 路由：

```text
registry.call()
  -> PtcFunction::RustAbi
  -> call_extension(extension, session, function, input)
```

Pi Extension 路由：

```text
registry.call()
  -> PtcFunction::JsExtension
  -> runtime_handle.execute_tool(...)
  -> extension execute(toolCallId, input, signal, onUpdate, ctx)
```

`tool_call_id` 由宿主生成，至少在进程内唯一。建议格式：

```text
ptc-{session}-{monotonic-sequence}
```

## 9. Extension Context 和 Hostcalls

第一阶段传递最小 context：

```json
{
  "cwd": "...",
  "mode": "print",
  "hasUI": false,
  "sessionId": "..."
}
```

支持：

- `ctx.cwd`
- `ctx.mode`
- `ctx.hasUI`
- `console`
- `pi.exec()`
- cancellation / timeout

不支持的 UI 和 session 方法必须返回明确错误，不能挂起 Promise。

执行 JS 工具时必须持续驱动 extension runtime 的 hostcall event loop；仅调用 `__pi_execute_extension_tool()` 后等待 Promise 会重现 child-process hostcall deadlock。

## 10. 并发和死锁规则

PTC runtime 与 Extension runtime 是两个独立执行域：

```text
PTC runtime
  -> NativeCall future
  -> Extension actor command
  -> Extension runtime execute()
  -> Extension hostcalls
```

约束：

- 不在持有 Extension runtime 状态锁时等待外部 hostcall。
- 不使用同一个全局 mutex 同时保护 PTC 和 Extension runtime。
- `NODE` 锁只保护当前 PTC 执行，不保护 Extension actor。
- 同一个 Extension runtime 第一阶段串行执行工具。
- 每次 JS 工具调用默认超时 60 秒，并支持取消。

## 11. PTC Tool Description

`node` tool description 改为列出统一模块目录：

```json
[
  {
    "module": "todo",
    "source": "rust",
    "functions": [
      {
        "name": "update",
        "input_schema": {},
        "output_schema": {}
      }
    ]
  },
  {
    "module": "my_extension",
    "source": "pi-extension",
    "functions": [
      {
        "name": "greet",
        "input_schema": {},
        "output_schema": {
          "type": "object",
          "properties": {
            "content": { "type": "array" },
            "details": {}
          }
        }
      }
    ]
  }
]
```

规则简化为：

```text
- 使用静态顶层 ES import。
- 调用所有函数时首选单个 object 参数。
- 所有函数都是 async，必须 await。
- Rust positional 调用只作为兼容，不在新提示中推荐。
- 输出 schema 是返回值的权威定义。
```

## 12. 文件级改动

### `e-agent-node-runtime/src/runtime.rs`

- `ExtensionToolDef` 增加 `extension_id`。
- 增加按 extension ID 加载、枚举和执行工具的宿主方法。
- 增加带 hostcall 驱动的 extension tool 执行方法。
- Native module wrapper 接受 object 参数，并保留 positional 参数转换。

### `e-agent-node-runtime/src/lib.rs`

- 导出 Extension runtime 所需类型和安全 handle API。

### `e-agent-core/src/tool/ptc/mod.rs`

- `LoadedExtension` 和 `call_extension()` 可移动到独立适配模块，或保持私有并由 registry 调用。
- 从统一 registry 生成 module metadata 和 `NativeCall`。
- 更新 `node` description。

### 新增 `e-agent-core/src/tool/ptc_registry.rs`

- `PtcModuleRegistry`。
- Rust/JS function adapters。
- 名称、冲突和 schema 校验。

### 新增 `e-agent-core/src/tool/js_extension.rs`

- `JsExtensionRuntimeHandle` actor。
- extension load/list/execute/shutdown。

### `e-agent-core/src/main.rs`

- 异步创建 executor。
- 读取 `E_AGENT_EXTENSION_PATHS`。
- 先加载 Rust tools，再加载 Pi extensions，最后启动 session。

## 13. 分阶段实施

### Phase 1：Object 参数兼容

- Native module 函数支持单 object 参数。
- 保留 Rust positional 调用。
- 更新 PTC description 和现有测试。

验收：

```ts
import { update } from "todo";
await update({ index: 0, status: "completed" });
await update(0, "completed");
```

两种写法结果一致。

### Phase 2：加载最小 Pi Extension

- 持久 Extension actor。
- 加载一个 `.ts/.js` entry。
- 执行 async factory。
- 收集 `registerTool()` metadata。

验收：工具 metadata 中包含 extension ID、name、description 和 parameters。

### Phase 3：将 Pi 工具暴露为 PTC module

- 注册 `PtcFunction::JsExtension`。
- PTC import 和调用。
- 返回完整 Pi `ToolOutput`。

验收：

```ts
import { greet } from "my_extension";
const result = await greet({ name: "Pi" });
console.log(result.content[0].text);
```

输出 `Hello Pi`。

### Phase 4：Rust 和 Pi Extension 混合

- 同一 registry 同时注册两类函数。
- 冲突检测。
- session 关闭时清理两类扩展状态。

验收：一个 PTC 程序先调用 Rust 工具，将结果传给 Pi Extension 工具。

### Phase 5：Hostcall、取消和可靠性

- `pi.exec()`。
- timeout、cancel 和 runtime shutdown。
- Extension runtime 崩溃后的错误隔离。

## 14. 最小测试集

### Node runtime 单元测试

- 加载同步和异步 extension factory。
- 一个 extension 注册多个工具。
- metadata 包含正确 extension ID。
- 按 `(extension_id, tool_name)` 执行正确闭包。
- JS throw 转 rejected Promise。
- object 和 positional 参数兼容。

### Core 集成测试

- 现有 Rust module tests 保持通过。
- TS extension tool 可被静态 import。
- Rust 和 TS modules 可在同一个 PTC 程序中调用。
- 两类模块重名时加载失败。
- 两个 extension 工具同名但模块不同，可分别调用。
- JS 扩展闭包状态跨两次 PTC 调用保留。
- `pi.exec()` 不死锁。
- session close 后 runtime 正常退出。

## 15. 验收标准

以下条款全部满足，才可认为第一版兼容层完成。

### AC-1：Rust PTC 回归兼容

给定现有 Rust `todo` 动态库：

```ts
import { update } from "todo";

await update(0, "completed");
```

执行成功，且结果与改造前一致。

### AC-2：Rust object 参数兼容

同一 Rust 函数可使用新参数形式：

```ts
import { update } from "todo";

await update({ index: 0, status: "completed" });
```

宿主传给 Rust ABI 的 JSON input 与 AC-1 等价。

### AC-3：加载标准 Pi Extension

给定一个 `.ts` extension：

```ts
export default function (pi) {
  pi.registerTool({
    name: "greet",
    description: "Greet a person",
    parameters: {
      type: "object",
      properties: { name: { type: "string" } },
      required: ["name"],
    },
    async execute(_id, params) {
      return {
        content: [{ type: "text", text: `Hello ${params.name}` }],
        details: { name: params.name },
      };
    },
  });
}
```

设置 `E_AGENT_EXTENSION_PATHS` 后，factory 成功执行，`greet` 可被枚举为该 extension module 的函数。

### AC-4：Pi 工具以 package/function 形式调用

PTC 程序可以调用 AC-3 注册的工具：

```ts
import { greet } from "my_extension";

const result = await greet({ name: "Pi" });
console.log(result.content[0].text);
```

stdout 包含 `Hello Pi`，并且 `result.details.name === "Pi"`。

### AC-5：混合组合

单个 PTC 程序可同时调用 Rust module 和 Pi Extension module：

```ts
import { list } from "todo";
import { greet } from "my_extension";

const items = await list({});
const result = await greet({ name: String(items.length) });
console.log(result.content[0].text);
```

两个调用均成功，且不要求工具实现语言出现在 PTC 程序中。

### AC-6：闭包状态保持

Pi extension 工具在两次独立的 `node` 工具调用间保留 module/factory 闭包状态：

```ts
let count = 0;

export default function (pi) {
  pi.registerTool({
    name: "next",
    description: "Return the next count",
    parameters: { type: "object", properties: {} },
    async execute() {
      count += 1;
      return { content: [{ type: "text", text: String(count) }], details: { count } };
    },
  });
}
```

第一次 PTC 调用返回 `1`，第二次返回 `2`。

### AC-7：隔离和冲突

- 相同 module 名加载失败，不发生静默覆盖。
- 同一 module 内相同 function 名加载失败，不发生静默覆盖。
- 不同 module 中同名函数可正常导入并调用。
- 未知 module 或 function 返回 rejected Promise，错误包含请求的 `module.function`。

### AC-8：错误、超时和关闭

- Pi tool 的 `execute()` 抛出异常时，PTC Promise reject，不导致进程崩溃。
- JS tool 到达默认超时后，调用结束并返回可定位的超时错误。
- extension 中的 `pi.exec()` 正常完成；不能出现 PTC/extension runtime 等待导致的死锁。
- `Session::close()` 后，Rust ABI session state 被清理，JS extension runtime shutdown，后续调用返回明确错误而非挂起。

### AC-9：自动化回归

至少新增以下可运行测试：

- Node runtime：同步/异步 factory、按 extension ID 执行、tool throw。
- Core：AC-1、AC-2、AC-4、AC-5、AC-6、AC-7。
- 现有 `e-agent-core` 与 `e-agent-node-runtime` 测试全绿。

### AC-10：官方 Pi Extension 的 release E2E

自动化测试不能替代完整可执行程序的验收。每个 release candidate 必须以 `cargo run --release` 运行官方 Pi 的真实 extension；不使用手写的最小 fixture 代替。

基础样例固定为官方仓库 `earendil-works/pi` 的 commit `e429d90b800f9a37c8a5812f4c9c10a8cdcc85a7`：

| 文件 | PTC module | 函数 | 覆盖的兼容面 |
|---|---|---|---|
| `packages/coding-agent/examples/extensions/todo.ts` | `todo` | `todo` | TypeScript factory、`typebox`/Pi package import、`registerTool`、state、`content`、`details` |
| `packages/coding-agent/examples/extensions/truncated-tool.ts` | `truncated_tool` | `rg` | Node `fs`/`os`/`path`/`child_process` imports、文件系统、真实 `rg` 子进程、工具结果 |

准备环境：

```powershell
$official = Join-Path $env:TEMP "pi-official-e429d90"
if (-not (Test-Path $official)) {
  git clone https://github.com/earendil-works/pi.git $official
}
git -C $official checkout e429d90b800f9a37c8a5812f4c9c10a8cdcc85a7

$sep = [IO.Path]::PathSeparator
$env:E_AGENT_EXTENSION_PATHS = @(
  "$official/packages/coding-agent/examples/extensions/todo.ts",
  "$official/packages/coding-agent/examples/extensions/truncated-tool.ts"
) -join $sep
```

前置条件：

- 正常 Agent 配置可用，即 `.env`、`E_MODULE_BIG` 和 provider 凭据已配置。
- `rg` 在 `PATH` 中。
- 当前工作目录为本仓库根目录。

Todo 全流程验收命令：

```powershell
cargo run --release -- 'Use the node tool exactly once. In that one program, import { todo } from "todo"; call await todo({ action: "add", text: "PTC Pi extension acceptance" }); then call await todo({ action: "list" }); print the list result text. Do not use any other tool.'
```

通过条件：

- 程序退出码为 `0`。
- 启动日志没有 extension load error、module resolution error 或未实现 API 错误。
- PTC stdout 包含 `PTC Pi extension acceptance`。
- 同一次 PTC 程序中的 `add` 后 `list` 可见新增项。

`truncated-tool.ts` 全流程验收命令：

```powershell
cargo run --release -- 'Use the node tool exactly once. In that one program, import { rg } from "truncated_tool"; call await rg({ pattern: "registerTool", path: "extensions.md" }); print result.content[0].text. Do not use any other tool.'
```

通过条件：

- 程序退出码为 `0`。
- PTC stdout 至少包含 `registerTool` 的匹配行。
- 没有 `Cannot find module`、`child_process`、`execSync`、hostcall timeout 或 deadlock 错误。

这两个命令必须加入 release 验收脚本或 CI 的可选 integration job；因其需要真实模型/provider，不放入默认离线单元测试。

### AC-11：带 npm 依赖的 package 扩展（后续门槛）

`pi-web-access` 是合适的后续真实样例，但它需要 package 安装和 npm runtime dependency resolution。只有在实现以下能力后，才把它加入硬验收：

- `package.json` runtime dependencies 解析。
- extension package entrypoint 解析。
- 网络访问的显式测试开关与稳定 mock/fixture。

届时验收应从 package entry 加载 `pi-web-access`，并在 PTC 中导入其公开 module/function 调用。它不属于第一版 `E_AGENT_EXTENSION_PATHS` 单文件兼容承诺。

## 16. 第一版完成标准

第一版完成时必须满足：

1. 不修改现有 Rust extension ABI。
2. 不要求修改标准 Pi extension 源码。
3. Rust positional PTC 程序继续运行。
4. 新 PTC 程序统一使用 `module.function({ ... })`。
5. Rust 和 Pi Extension 工具可在同一程序中组合。
6. Pi Extension 的 JS 状态跨调用保留。
7. 错误、超时和 runtime shutdown 不挂起 Agent。
8. 第 15 节的 AC-1 至 AC-10 全部通过；AC-11 在 package loading 范围实现后转为硬门槛。

后续功能只在出现真实需求时增加；第一版不实现顶层 LLM tool 暴露、完整 Pi lifecycle 或通用工具间调用。
