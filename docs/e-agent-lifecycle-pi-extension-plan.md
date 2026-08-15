# E-Agent 生命周期与 Pi Extension 适配计划

## 目标

让 E-Agent 成为生命周期、会话和消息调度的唯一真源；Pi Extension 层只负责把 E-Agent 事件与能力转换成 Pi API。

首个完整验收对象是 `@narumitw/pi-goal@0.51.0`：支持 `/goal` 启动、自动续跑、完成和进程重启后恢复。现有 Rust ABI 工具和 `pi-web-access` 行为不得回退。

## 参考与取舍

参考 `Dicklesworthstone/pi_agent_rust@b344bb3f00ade453ae6fd51a3f843888d1bdd28f` 的核心做法：

- Agent 主循环按稳定顺序产生 run、turn、message、tool 事件。
- Extension manager 负责命令与事件分发，不拥有 Agent 状态。
- `ExtensionSession` 与 `ExtensionHostActions` 分离：前者读写持久会话，后者投递 steer/follow-up 消息。
- 宿主拥有消息队列、session persistence 和 idle/settled 语义。

不照搬其完整扩展、安全、UI、RPC 和分支系统；只实现当前 E-Agent 与 `pi-goal` 必需的最小闭环。

## 架构边界

```text
Session (状态与主循环)
  ├─ SessionStore       消息和 extension entries
  ├─ MessageQueue       steer / follow-up
  ├─ Lifecycle          E-Agent 事件协议
  └─ ExtensionHost      commands / hooks / tools / host actions
                              ↓
                       PiExtensionAdapter
                              ↓
                         PiJsRuntime
```

规则：

1. `Session` 决定事件顺序并驱动 Agent。
2. `SessionStore` 是持久状态唯一真源；QuickJS 内存只保存扩展闭包和短期运行状态。
3. `MessageQueue` 决定下一轮何时开始；扩展不能直接递归调用 Agent。
4. Pi adapter 不定义第二套生命周期，只做类型和名称映射。

## 核心 trait

### `LifecycleHook`

```rust
#[async_trait::async_trait(?Send)]
pub trait LifecycleHook {
    async fn dispatch(
        &self,
        event: LifecycleEvent,
        ctx: &SessionContext<'_>,
    ) -> anyhow::Result<LifecycleEffect>;
}
```

`LifecycleEvent` 第一阶段只包含：

```text
SessionStart, Input, BeforeAgentStart,
AgentStart, TurnStart, MessageStart, MessageEnd,
ToolCall, ToolExecutionEnd, TurnEnd, AgentEnd,
AgentSettled, SessionShutdown
```

`LifecycleEffect` 只提供当前确有消费者的三种结果：

```text
None
TransformInput { text }
BeforeAgentStart { system_prompt, messages }
BlockTool { reason }
```

不要提前设计通用 middleware、优先级或并行 dispatch。

### `SessionStore`

```rust
pub trait SessionStore {
    fn id(&self) -> SessionId;
    fn messages(&self) -> &[Message];
    fn entries(&self) -> &[SessionEntry];
    fn append_message(&mut self, message: Message) -> anyhow::Result<()>;
    fn append_custom(&mut self, kind: String, data: serde_json::Value) -> anyhow::Result<()>;
    fn save(&self) -> anyhow::Result<()>;
}
```

`SessionEntry` 先支持 `Message` 和 `Custom`。采用 JSONL；每次 append 同步写入，避免另建缓存、数据库或后台刷盘。默认目录通过 `dirs::home_dir()` 定位到 `~/.e/sessions/`，不手写 HOME/USERPROFILE 平台判断。

### `MessageSink`

```rust
pub trait MessageSink {
    fn enqueue(&mut self, message: QueuedMessage) -> anyhow::Result<()>;
    fn is_idle(&self) -> bool;
}
```

`QueuedMessage` 支持 `Steer` 和 `FollowUp`。`NextTurn` 暂按 `FollowUp` 处理并写清语义，不增加第三套调度分支。

### `ExtensionHost`

组合已有工具能力，不替换 `ToolExecutor`：

```rust
#[async_trait::async_trait(?Send)]
pub trait ExtensionHost: LifecycleHook {
    fn commands(&self) -> Vec<CommandDef>;
    async fn command(
        &self,
        name: &str,
        args: &str,
        ctx: &SessionContext<'_>,
    ) -> anyhow::Result<()>;
}
```

`ProgrammaticToolExecutor` 实现该 trait，并继续实现现有 `ToolExecutor`。

## 建议文件布局

```text
e-agent-core/src/
  lifecycle.rs             事件、effect、LifecycleHook（约 150 行）
  session/
    mod.rs                 Session 编排（控制在 250 行内）
    store.rs               JSONL store、SessionEntry（约 200 行）
    queue.rs               steer/follow-up（约 120 行）
  tool/
    mod.rs                 保留 ToolExecutor
    extension.rs           CommandDef、ExtensionHost（约 100 行）
    ptc/
      mod.rs               catalog/路由
      pi_actor.rs          Pi runtime actor 命令（从 mod.rs 拆出）
      pi_adapter.rs        event/context/hostcall 映射
```

目标是拆分现有 `tool/ptc/mod.rs`，不是增加同义 wrapper。单文件软上限 300 行；PiJS shim 大文件暂不在本计划重构。

## 分阶段实施

### 阶段 1：E-Agent 生命周期骨架

- 新增 `LifecycleEvent`、`LifecycleEffect`、`LifecycleHook`。
- `Session::run_one_trun` 改为稳定事件顺序：

```text
AgentStart
  TurnStart
    MessageStart → MessageEnd
    [ToolCall → ToolExecutionEnd]*
  TurnEnd
AgentEnd
AgentSettled
```

- `SessionStart` 在首次运行前发一次；`SessionShutdown` 在 `close()` 发一次。
- provider/tool 失败仍保证已开始的外层事件有对应结束事件，错误放入结束事件；不做复杂 unwind 状态机。

**验收：**

- 一个小测试记录事件数组，精确等于上述顺序。
- 有一次工具调用时，第二个 turn 出现且 Tool 事件包住实际调用。
- `cargo test -p e-agent` 通过。

### 阶段 2：会话存储与消息队列

- 增加最小 JSONL `SessionStore`，默认持久化到 `dirs::home_dir()/.e/sessions/<session-id>.jsonl`；CLI 可指定已有 session 文件恢复。
- `Session` 不再直接拥有裸 `Vec<Message>`，改由 store 提供上下文。
- 实现 steer/follow-up 队列：steer 在当前 tool turn 后优先消费；follow-up 在 Agent settled 后启动新 run。
- `append_custom` 不进入 LLM messages。

**验收：**

- 关闭并用同一路径重开 session 后，messages 和 custom entries 一致。
- follow-up 在当前 run 的 `AgentEnd` 后触发下一次 `AgentStart`。
- custom entry 不出现在 provider context。

### 阶段 3：命令与 Extension Host

- 增加 `CommandDef`、`ExtensionHost`。
- 输入以 `/name` 开头时先查 extension command；命中则不直接发送给 provider。
- Pi actor 增加 `GetCommands`、`ExecuteCommand`、`DispatchEvent`；复用 runtime 已有 `__pi_execute_command` 和 `__pi_dispatch_extension_event`。
- actor 仍维持单线程、单 runtime，不新建 runtime pool。

**验收：**

- fixture extension 注册 `/echo`，命令收到完整 args 且 provider 未被调用。
- `session_start` 和 `agent_start` 能到达 fixture hook，顺序与 E-Agent 事件一致。
- 未知 `/name` 保持普通用户输入语义。

### 阶段 4：Pi context 与宿主动作

将 E-Agent 数据映射到 Pi ctx：

```text
sessionManager.getSessionId  ← SessionStore::id
getEntries/getBranch         ← SessionStore::entries
isIdle                       ← MessageSink::is_idle
cwd                          ← Session cwd
```

处理 Pi host actions：

```text
appendEntry      → SessionStore::append_custom
sendUserMessage  → MessageSink::enqueue
sendMessage      → append custom/message；triggerTurn 时 enqueue
```

`ui.setStatus/notify` 在无 TUI 模式输出到 stderr 或 no-op；不为本阶段建立 UI trait。

**验收：**

- fixture 在 `session_start` 读取真实 session id 和 entries。
- `appendEntry` 写入 JSONL，重启后仍能读取。
- `sendUserMessage(... followUp)` 只在当前 Agent settled 后启动下一轮。
- hostcall 错误返回扩展，不再被 PTC 的 `unsupported` 吞掉。

### 阶段 5：Pi 事件映射

建立一张显式映射表，不让 Pi 名称进入 Session 主循环：

```text
E-Agent                  Pi
SessionStart             session_start
Input                    input
BeforeAgentStart         before_agent_start
AgentStart               agent_start
MessageStart             message_start
ToolCall                 tool_call
ToolExecutionEnd         tool_execution_end
TurnEnd                  turn_end
AgentEnd                 agent_end
AgentSettled             agent_settled
SessionShutdown          session_shutdown
```

应用返回值：

- `input` 可 transform/handled。
- `before_agent_start` 可修改 system prompt、注入 messages。
- `tool_call` 可 block。
- 其他事件仅观察。

**验收：**

- fixture 分别验证 input transform、system prompt 修改、tool block。
- 多扩展按加载顺序串行执行；前一个 transform 是后一个输入。
- 不增加事件重试；handler 报错带扩展 ID 返回当前操作。

### 阶段 6：简单多轮 CLI

- 当前单 prompt 参数改为可选；有参数时先执行该 prompt，随后从 stdin 逐行读取下一轮。
- 空行继续等待，EOF 或 `/exit` 正常关闭 session。
- 保持普通 stdout/stderr，不做历史、补全、光标控制或全屏 TUI。
- 该阶段只提供人工验收入口，不扩展成独立交互框架。

**验收：**

- 管道输入两行 prompt 时按顺序完成两个用户轮次，并复用同一 session。
- `/exit` 触发 `SessionShutdown`，session 文件可再次加载。

### 阶段 7：真实扩展 LLM 端到端验收

同时加载原版、未修改源码的：

- `@narumitw/pi-goal@0.51.0`
- `pi-web-access@0.22.0`

由真实 LLM 执行以下完整任务，而不是由测试代码直接调用扩展函数：

```text
/goal 使用 web_search 查找 OpenAI 官方 About 页面，再用 fetch_content 读取页面；
只依据抓取内容报告页面标题、OpenAI 的使命和最终 URL，并在逐项核对完成后结束 goal。
```

流程必须真实经过：

```text
/goal command
→ session_start / input / before_agent_start
→ LLM 收到 goal prompt
→ LLM 调用 web_search
→ LLM 调用 fetch_content
→ tool lifecycle events
→ 至少一次 follow-up 自动续跑
→ LLM 使用当前 goal_id 调用 goal_complete
→ agent_end / agent_settled
```

再执行恢复场景：在 `goal_complete` 前终止进程，使用同一 session 文件重启，由 LLM 继续并完成同一个 goal。

**最终完成标准（全部满足才通过）：**

1. 两个扩展均从发布包加载，验收过程不修改其源码或用 wrapper 改写行为。
2. `/goal` 由命令路由创建 active goal；日志中没有 `no active goal`。
3. `web_search` 和 `fetch_content` 都由 LLM 自主调用且返回成功；不得用 Rust 测试或手写 PTC 程序代替。
4. 最终回答中的标题、使命和 URL 可在 `fetch_content` 返回正文中逐项对应。
5. Goal 至少跨两个 Agent run，follow-up 由 E-Agent 队列投递。
6. LLM 使用当前 `goal_id` 调用 `goal_complete`，结果为成功而非 in-band rejection；完成后无额外续跑。
7. 中途退出并重启后，恢复相同 goal id、目标和迭代状态，LLM 能继续调用 web access 并完成。
8. 运行记录能看到 command、session、agent、tool、follow-up、completion 的关键事件顺序，且无 extension error、unsupported hostcall 或未处理异常。
9. 同一构建连续运行完整场景 3 次全部通过，排除一次性偶然成功。
10. `cargo test --workspace`、`cargo fmt --all -- --check`、`git diff --check` 通过。

以上是本迭代“Goal + Web Access 100% 正常”的定义；任一项失败都不算完成。

## 明确不做

- 不实现完整 Pi TUI、shortcut、provider、MCP、flags。
- 简单多轮 CLI 仅支持逐行输入、EOF 和 `/exit`；不做 readline、历史、补全或终端 UI。
- 不实现 session branching/tree/compaction；等 E-Agent 自身需要时再加。
- 不做通用事件优先级、并行 handler、自动重试或事件持久化。
- 不新增数据库、消息 broker 或 runtime pool。
- 不追求全部 Pi extension parity；以真实 `pi-goal` 闭环和现有扩展不回退为边界。

## 已确认的实施决定

1. Session 默认保存在 `dirs::home_dir()` 下的 `~/.e/sessions/`；使用 `dirs` crate 处理平台 home 目录。
2. 提供简单逐行多轮 CLI，但它不是本次迭代重点，不引入交互框架。
