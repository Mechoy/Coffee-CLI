# Coffee CLI 多 Agent 工作区恢复设计

> 文档状态：Implemented / 初始实现已完成，待真实 CLI 端到端验收
>
> 文档版本：1.0
>
> 更新日期：2026-08-09
>
> 适用范围：Coffee CLI 桌面端的 `multi-agent`、`two-agent`、`three-agent` 协作标签页

---

## 1. 决策摘要

Coffee CLI 应增加的是**可恢复的多 Agent 工作区快照**，不是恢复已经销毁的 PTY，也不是复制各 AI CLI 的聊天记录。

关闭 Coffee CLI 后，Claude、Codex、OpenCode 等原生 CLI 自己负责保存各自的会话历史；Coffee 负责保存的是将这些原生会话重新组成一个协作网格所需的最小元数据：Pane 顺序、工具、工作目录、MCP 选择、通知开关及可验证的原生会话标识。

恢复时必须创建新的 Coffee tab、Pane PTY、`runId`、MCP endpoint 和协调器 epoch，然后通过原生 CLI 的 resume 参数重新连接每个已验证的会话。旧的任务队列、端口、临时文件和终端滚屏不恢复。

最重要的安全原则：**绝不按“同目录最新会话”猜测一个 Pane 应恢复到哪个 Codex/OpenCode 会话。** 多个 Pane 可同时使用同一工具和目录，猜错会把两个独立对话接在一起，比无法自动恢复更糟。

---

## 2. 问题与现状

### 2.1 用户问题

当前用户可以在一个协作标签页中同时运行 2、3 或 4 个 Agent。关闭 Coffee CLI 后：

1. Coffee 的多 Pane 网格消失；
2. 下次启动只出现新的空终端；
3. 用户即使能在历史列表中找到某个原生会话，也必须逐个手工恢复；
4. 原先的工具组合、目录、MCP 选择和协作上下文无法作为一个整体回到界面中。

这不是“聊天记录找不到”的单一问题，而是 Coffee 没有保存**协作工作区拓扑**。

### 2.2 当前实现的事实

| 事实 | 当前位置 | 影响 |
|---|---|---|
| 多 Agent 拓扑由后端 snapshot 保存 | `src/multi_agent_workspace.rs` | 重启后可从恢复面板一次恢复 2/3/4 Pane 网格。 |
| 单会话历史可通过 `resumeToken` 恢复 | `HistoryBoard.tsx` -> `TierTerminal.tsx` -> `server.rs` | 现有的安全 resume argv 构建和 token 格式校验被多 Agent 恢复复用。 |
| 多 Pane 启动先由后端建立恢复租约，再创建 PTY | `multi_agent_workspace.rs`、`server.rs` | 某个 Pane 的失败、取消或旧渲染器回调不会误结算其他 Pane。 |
| 运行期 `runId`、PTY、MCP endpoint、任务协调器均是内存对象 | `terminal.rs`、`mcp_server.rs`、`server.rs` | 退出后它们必然失效，恢复时必须创建新的运行期对象。 |
| 关闭应用会主动终止子进程并清理每 Pane 临时 MCP 产物 | `server.rs` 的 `ExitRequested` 路径 | 不存在“原样继续运行的终端进程”；恢复必须新建进程。 |

### 2.3 与现有单会话恢复的关系

现有 `tier_terminal_start` 已能在严格校验 token 和 cwd 后构造原生恢复命令，例如：

```text
claude --resume <uuid>
codex resume <uuid>
opencode --session <ses_id>
```

恢复工作区应复用这个启动路径，而不是新增一套绕开校验的 shell 命令拼接。MCP 会话参数也必须在 resume argv 构造完成后按现有路径重新注入。

---

## 3. 产品边界

### 3.1 要解决的问题

- 在下一次启动 Coffee 时列出此前未显式关闭的多 Agent 工作区。
- 以一个动作恢复一个 2/3/4 Pane 的协作网格，而非逐个从历史列表打开。
- 在每个 Pane 中尽可能恢复到正确的原生 CLI 对话。
- 保留工作目录、Pane 编号、工具、MCP 请求选择和通知开关。
- 对无法验证的 Pane 明确显示原因，并允许用户逐 Pane 决定新建、手工绑定历史会话或跳过。
- 恢复后建立全新的 Coffee 协作轮次，使旧运行期任务不会污染新窗口。

### 3.2 明确不做

- 不恢复旧 PTY、PID、WebView、xterm scrollback 或终端文本回放。
- 不保存或重放 `TaskCoordinator` 的 running job、完成通知、结果路径或 `send_to_pane` 调用。
- 不让 Coffee 在应用关闭后常驻后台运行 Agent。
- 不复制原生 CLI 的完整会话文件，也不把对话内容导入 Coffee 数据库。
- 不保存 MCP 密钥、Header 值、环境变量值、临时 MCP 配置、端口或 `COFFEE_AGENT_RESULTS_DIR`。
- 不在没有用户确认时冷启动 2 至 4 个 Agent；恢复会启动模型、MCP 和外部工具，存在成本和权限影响。
- P0 不扩展到独立 Split Pane 或所有单 Agent tab；数据模型可以预留扩展点，但实现聚焦协调型多 Agent。

---

## 4. 核心原则

1. **原生会话与 Coffee 工作区分层。** 原生 CLI 保存对话，Coffee 保存协作拓扑和恢复引用。
2. **错误恢复比缺失恢复更危险。** 没有确定身份时必须停在“待绑定”，不能猜测。
3. **运行期身份不可复用。** 每次恢复生成新的 tab、session、`runId`、MCP endpoint 和协调 epoch。
4. **结构状态持续落盘。** 不能仅依赖 `beforeunload`、窗口关闭事件或应用退出钩子。
5. **恢复需显式确认。** 避免启动时悄悄发起模型请求、连接 MCP 或触发外部工具。
6. **单 Pane 失败隔离。** 一个 cwd、工具、Profile 或 token 失效不得阻止其他 Pane 恢复。
7. **存储最小化且有上限。** 保存引用和配置，不保存对话、输出或无限增长的日志。

---

## 5. 术语与恢复语义

| 名称 | 含义 |
|---|---|
| Workspace Snapshot | Coffee 保存的一条协作工作区元数据记录。 |
| Pane Slot | 用户可见的 `pane-1` 到 `pane-N`，恢复后编号保持不变。 |
| Native Session | Claude、Codex、OpenCode 自己保存的会话。Coffee 只保存其恢复引用。 |
| Resume Identity | 已验证的 `{ tool, token, cwd }` 组合，用于构造原生 resume。 |
| Coordination Epoch | 一次 Coffee 进程内协作运行的代际标识。恢复必定创建新 epoch。 |
| Restore Plan | 恢复前预检后的逐 Pane 启动计划及状态。 |

恢复的成功定义是：Coffee 建立新的协作网格，并让每个状态为“可恢复”的 Pane 启动到其**正确的**原生会话。它不意味着旧进程、终端缓冲区或正在执行的跨 Pane 任务继续存在。

---

## 6. 持久化模型

### 6.1 存储位置与所有权

新增后端模块，例如：

```text
src/multi_agent_workspace.rs
~/.coffee-cli/multi-agent-workspaces.json
```

前端只发起保存、读取、预检、恢复和删除命令；后端是磁盘文件的唯一所有者。不能用 `localStorage` 作为权威来源，因为 WebView 数据可能被清理，而且无法提供原子写入、文件权限、并发控制和损坏恢复。

文件使用独立版本号、revision、互斥锁、临时文件加原子 rename。实现应复用 `mcp_config.rs` / `tool_config.rs` 的安全写入经验，并为损坏文件保留只读备份和可见恢复动作。

建议的磁盘约束：

- 配置目录仅用户可读写；Unix 文件权限为 `0600`。
- 总文件大小上限 1 MiB，单 snapshot 上限 64 KiB。
- 默认最多保留 32 个未明确丢弃的工作区记录；达到上限时要求用户清理，不能静默删除最近恢复记录。
- 不写终端输出，因此正常使用不随对话长度线性占用磁盘。

### 6.2 建议 Schema

```json
{
  "version": 1,
  "revision": 14,
  "workspaces": [
    {
      "snapshot_id": "a5ed4b51-4e9a-4c19-a4d5-997063b2bb5d",
      "saved_at": "2026-08-09T09:30:00Z",
      "pane_count": 4,
      "workspace": "/Users/me/project",
      "panes": [
        {
          "pane_index": 1,
          "tool": "claude",
          "sentinel_enabled": true,
          "mcp_selection": { "mode": "profile", "profile_id": "research" },
          "continuation": {
            "state": "known",
            "token": "a1b2c3d4-...",
            "source": "runtime_capture",
            "observed_at": "2026-08-09T09:29:41Z"
          }
        },
        {
          "pane_index": 2,
          "tool": "codex",
          "sentinel_enabled": true,
          "mcp_selection": { "mode": "auto" },
          "continuation": { "state": "needs_binding" }
        }
      ]
    }
  ]
}
```

真实类型应使用 Rust enum 和 Serde tagged union，而不是依赖前端自由 JSON。字段必须受到长度、Pane 编号、工具白名单、绝对路径、Profile ID、token 格式和记录数量校验。

### 6.3 每 Pane 的 continuation 状态

```text
known(token, source, observed_at)
needs_binding(reason)
fresh_by_user
unsupported(reason)
```

- `known`：token 已由权威方式捕获或由用户从历史列表显式绑定；可进入自动恢复预检。
- `needs_binding`：工作区拓扑可以恢复，但 Coffee 还不能证明哪个原生会话属于此 Pane。
- `fresh_by_user`：用户明确选择恢复网格但以新会话启动该 Pane。
- `unsupported`：当前工具没有受支持的原生 resume 合同；不能伪装为已恢复。

### 6.4 必须排除的字段

以下数据永远不进入 snapshot：

- `runId`、PID、PTY session、端口、MCP URL、临时目录和 artifact 路径。
- 任务 job ID、pending task、完成事件、结果文本、自动注入消息。
- xterm scrollback、完整对话、模型输出、截图和文件内容。
- MCP server 定义、环境变量实际值、Header 实际值、密码、Cookie、OAuth token。
- 未审计的 `toolData`。协调型多 Agent 当前只允许 Claude/Codex/OpenCode，P0 不需要持久化可能携带敏感信息的连接参数。

---

## 7. 原生会话身份策略

### 7.1 不可接受的做法

以下规则一律禁止作为自动绑定依据：

```text
同一个 cwd + 最新修改时间
同一个工具 + 最近一条历史
同一时间窗口内任意新建 session
失败后退化为 CLI 的 --continue / --last
```

四个 Pane 可以同时启动相同的 CLI、使用相同目录，甚至存在另一个 Coffee 实例外的原生 CLI。上述规则无法证明归属关系。

### 7.2 当前可信能力矩阵

| 工具 | 原生 resume 形式 | 当前运行中 token 来源 | 自动恢复资格 |
|---|---|---|---|
| Claude | `claude --resume <uuid>` | Coffee 在新建时生成 UUID 并传入 `--session-id`，恢复前验证同 cwd 下的精确原生 JSONL 会话 | 已实现严格自动恢复。 |
| Codex | `codex resume <uuid>` | 当前 PTY 不输出可用 token；历史解析可读取 token | 未证明单 Pane 身份关联前，不自动按历史猜测。 |
| OpenCode | `opencode --session <ses_...>` | 当前 PTY 不输出可用 token；仓库可从历史读取 | 本机未安装，且未证明单 Pane 身份关联前，不自动猜测。 |

P0 只对 `known` token 自动恢复。Codex/OpenCode 的自动恢复必须先完成一个可复现的 identity-capture spike：在同一 cwd 并发启动多个同工具 Pane 后，仍能把每个原生 session 无歧义地绑定回其 Pane。未通过该门禁时，产品界面只提供“从历史手工绑定”“新建该 Pane”“跳过”。

### 7.3 Token 生命周期

1. Pane 启动时，后端 Runtime Registry 将当前 Coffee Pane 与持久化 snapshot 关联；新 Claude Pane 使用 Coffee 生成的 UUID 作为原生 `--session-id`。
2. Claude 的 UUID 只有在恢复预检中同时通过工具、cwd 和精确原生 JSONL 身份验证后，才以 `known(source=managed_claude_session)` 自动恢复。
3. 用户从历史选择某个会话作为该 Pane 的继续对象时，后端验证工具、cwd 和 token 格式后写入 `known(source=manual_binding)`。
4. 恢复前通过新的按工具定向验证 API 预检 token、cwd、CLI 可用性和 MCP Profile；不能只依赖最多 200 条的全局历史列表。
5. 恢复后，新运行如果产生新的原生 token，必须覆盖旧 token，保证下一次恢复指向最新的继续分支。

---

## 8. 快照与退出协议

### 8.1 写入时机

快照必须在以下结构变化后先完成后端持久化，再让前端提交对应状态变更；普通编辑可用小型去抖写入（例如 250 至 500 ms）作为补充。这样关闭 Pane、修改 MCP 选择或 Sentinel 开关后立即退出，也不会把旧拓扑恢复回来。成功恢复、用户显式关闭 tab、用户修改 Pane 设置后同样立即收敛：

- 创建或删除协调型多 Agent tab。
- 选择、清空或替换 Pane 工具。
- 工作目录、Pane 数量、MCP 选择、通知开关变化。
- 权威 token 捕获或用户手工绑定变化。
- 用户选择将 Pane 以后按新会话启动。

退出流程只能作为额外 flush，不能是唯一保存点：强制退出、崩溃、操作系统终止和 macOS 的隐藏窗口语义都不能保证前端退出 IPC 成功。

### 8.2 用户操作语义

| 操作 | Snapshot 行为 |
|---|---|
| 关闭整个 Coffee 应用 | 保留所有尚未显式丢弃的多 Agent workspace。 |
| macOS Cmd+W / 隐藏主窗口 | 不改变 snapshot，也不终止工作区。 |
| 关闭一个 Pane | 将该 slot 保存为空或对应的显式选择，不保留其旧 token。 |
| 关闭整个多 Agent tab | 保留 workspace snapshot，与单会话历史一致；用户可在恢复面板再次恢复。只有“丢弃工作区”才删除。 |
| 恢复成功后再次退出 | 更新同一 `snapshot_id`，不无限新增副本。 |
| 用户点击“丢弃工作区” | 原子删除 snapshot；不删除原生 CLI 历史。 |

---

## 9. 恢复流程与界面

### 9.1 启动入口

应用启动后读取 snapshot，但不自动挂载或启动 Agent。若存在记录，在中心启动页或非阻塞恢复面板显示“恢复协作工作区”。每张卡片展示：

- 工作目录和保存时间。
- 2/3/4 Pane 拓扑。
- 每 Pane 的工具和恢复状态。
- MCP Profile 是否仍存在。
- 明确操作：恢复、逐 Pane 配置、丢弃。

恢复按钮需要用户主动点击，避免无感启动多个模型会话、MCP server 或高权限工具。

### 9.2 预检状态

恢复前后端对每 Pane 返回清晰状态：

```text
resumable          token、cwd、工具、Profile 均通过预检
needs_binding      缺少确定 token，需要从原生历史显式选择
fresh_choice       用户已选择以新会话启动
tool_missing       当前设备找不到 CLI
cwd_missing        原目录不存在，拒绝恢复到错误目录
profile_missing    请求的 MCP Profile 已删除或无效
token_invalid      token 格式或原生会话验证失败
skipped            用户选择本次不启动该 Pane
```

默认“恢复全部可用 Pane”只能启动 `resumable` 的 Pane。其余 Pane 保留在网格中显示状态，用户可以逐个处理。不能悄悄降级到空会话，也不能让一个失败 Pane 阻断其他 Pane。

### 9.3 启动顺序

1. 创建新的 Coffee tab ID、每 Pane session ID 与 `runId`；不得重用旧运行期身份。
2. 将恢复计划放入 reducer，先显示网格和逐 Pane 状态。
3. 通过现有 `tier_terminal_start` 为每个可启动 Pane 调用原生 resume 或经用户确认的新建启动。
4. 由现有启动路径为每个新 run 创建 MCP endpoint、临时注入配置和终端事件订阅。
5. 每 Pane 独立报告启动成功或失败；已启动 Pane 不因同组失败而被杀掉。
6. 成功或失败结果写回 snapshot，供下一次恢复使用。

P0 不提供“恢复快照副本”的并行功能。一个 snapshot 在同一运行期只能恢复一次，避免同一个原生会话被重复启动到两个 Pane。

---

## 10. 多 Agent 协调与 MCP 语义

恢复后的协作系统必须视为新代际：

- 新建 `TaskCoordinator` 状态，不恢复旧 running/failed/completed job。
- 新建 per-pane MCP endpoint、artifact 目录和 `runId`；现有 run-generation 校验继续阻止旧事件写入新 Pane。
- 恢复时再次注入 Coffee 协作协议，并追加简短说明：旧协作轮次的 job ID、临时结果和未完成派发已失效；需要先 `list_panes`，再重新派发任务。
- 不自动重放 `send_to_pane`、`complete_task`、`read_pane` 或任何旧任务结果。
- Pane 编号保持 `pane-1` 至 `pane-N`，但其 Coffee 路由身份和协调 epoch 是新的。

MCP 的恢复原则：

- 只持久化请求选择 `Auto`、`None` 或 `Profile(id)`，不持久化解析后的 server、secret 或注入文件。
- 恢复时按当前 MCP 配置重新解析；这避免保存密钥，却意味着 Profile 删除或变更可能使恢复计划失效。
- 若 Profile 不存在，显示 `profile_missing`，要求用户显式改为另一个 Profile、`None` 或跳过；不能默认降为 Auto，因为权限面可能变化。

---

## 11. 实施拆分

### 当前实现状态（2026-08-09）

- Phase 0 的 Claude 身份路径已落实为 Coffee 管理的 UUID 加精确本地会话验证；Codex/OpenCode 仍坚持手工绑定或新建，不按“最新会话”猜测。
- Phase 1 至 Phase 3 的快照存储、恢复入口、逐 Pane 恢复租约、失败隔离、渲染器代际隔离和恢复执行均已实现。
- Rust 单元测试、`cargo check` 与前端生产构建已通过；Phase 4 的真实 Tauri 加真实 CLI 崩溃/重启 smoke test 仍需在目标机器完成。

### Phase 0：原生身份验证 Spike

目标：证明每个目标 CLI 能否在同 cwd、多 Pane 并发场景下，确定性地获得“本 Pane 的原生 session token”。

- 对 Claude 验证运行期 token 捕获、退出后 resume 和同 cwd 并发场景。
- 对 Codex 研究是否存在权威 launch/session 标识或可验证的文件关联；不得使用时间窗口猜测。
- 对 OpenCode 在实际安装版本上验证 resume 参数和 session 存储格式。
- 产出 CLI 能力矩阵与 fixture；任何未通过的 CLI 停留在 `needs_binding`。

门禁：未证明 identity capture 的 CLI 不得启用自动会话恢复。

### Phase 1：后端快照存储与纯测试

- 新建 `multi_agent_workspace` 模块、版本化 schema、revision、原子写入、文件权限、损坏备份和并发写保护。
- 实现 snapshot CRUD、容量限制、路径与 DTO 校验。
- 引入 Snapshot Runtime Registry，用当前 Pane 运行信息将可信 token 写回持久记录。
- 新增针对文件损坏、并发覆盖、迁移、删除、容量和敏感字段排除的单测。

### Phase 2：恢复计划与前端状态

- 扩展 `MultiAgentPane`：增加运行时 `resumeToken` / `restoreState`，但不把前端内存作为持久化权威。
- 增加后端 commands：列出快照、预检、手工绑定、恢复、丢弃、增量 checkpoint。
- 在 `CenterPanel` 增加恢复入口和 `RESTORE_MULTI_AGENT_WORKSPACE` reducer action；替换启动时多余的默认空 tab。
- 在 `MultiAgentGrid` 将 Pane 的 resume token 传给 `TierTerminal`。

### Phase 3：恢复执行、epoch 与故障隔离

- 通过既有 `tier_terminal_start` 走 token 校验、resume argv 构建和 MCP 注入。
- 让 `multi_agent_protocol` 接收新的 coordination epoch 并提示已恢复会话。
- 支持逐 Pane 失败状态、手工绑定历史、显式新建和跳过。
- 确保 tab/PID/run/endpoint/artifact 的生命周期继续遵循现有 generation isolation。

### Phase 4：端到端验证与产品收敛

- 使用可控的假 CLI fixture 测试 2/3/4 Pane 同 cwd 恢复、失败隔离和重复恢复拒绝。
- 对每个实际支持的 CLI 做本机手工 smoke test。
- 增加恢复卡片的无障碍、国际化和空状态测试。
- 只有所有恢复语义和磁盘边界通过后，才考虑将相同基础设施扩展到独立 Split 或单 Agent tab。

---

## 12. 验收标准

### 12.1 功能

- 关闭并重启后，用户能看到此前未关闭的 2/3/4 Pane 协作工作区。
- `known` 的 Pane 使用正确工具、正确 cwd 和正确原生 resume argv 启动。
- 所有 Pane 恢复后仍能使用 Coffee 多 Agent MCP 相互发现和通信。
- 已删除的 MCP Profile、缺失 cwd、缺失 CLI 或无效 token 只影响对应 Pane。
- 用户能为 `needs_binding` Pane 从历史明确选择会话，或明确选择新建/跳过。

### 12.2 正确性与安全

- 两个以上同 cwd、同工具 Pane 不会因“最新会话”规则发生错误匹配。
- 恢复后旧 run 的输出、退出、任务完成事件不能影响新 Pane。
- 不会自动重放旧任务或将旧 `complete_task` 接受为新协调器任务。
- snapshot 不包含终端输出、任务文本、密钥、MCP Header/环境值、端口、临时路径或 `runId`。
- snapshot 损坏、并发写入和未知未来版本不会静默覆盖数据。

### 12.3 资源

- 正常关闭、强制关闭和连续重启不会产生无限增长的快照或临时文件。
- snapshot 文件大小受固定上限控制，不随原生对话长度增长。
- 恢复不会留下孤儿子进程；重复点击恢复不会启动同一 snapshot 的第二组 Pane。

---

## 13. 已知风险与待确认项

| 风险或问题 | 处理原则 |
|---|---|
| Codex/OpenCode 缺少运行期 token | 先做 identity spike；未验证前只允许手工绑定或新建。 |
| 原生 CLI 升级改变历史文件格式或 resume 参数 | adapter 按工具独立测试；不能由通用字符串解析兜底。 |
| 原目录被移动或删除 | 拒绝自动 resume 到 home 或相似目录，要求用户显式处理。 |
| MCP Profile 已变化 | 重新解析当前配置，展示 drift；不悄悄扩大或缩小权限。 |
| 用户关闭 app 时 Pane 正在工作 | 保存最后一致拓扑，但把协作任务视为中断；不承诺自动续跑。 |
| snapshot 中的 cwd 和 token 属于本地敏感元数据 | 最小字段、用户级权限、无遥测上传、显式删除入口。 |
| 多次恢复同一工作区 | 同一运行期禁止并行恢复同一 snapshot；后续“复制工作区”需单独设计。 |

---

## 14. 复核结论

该功能有明确价值：它把“每个 CLI 各自有历史”提升为“整个协作现场可以恢复”，解决用户关闭 Coffee 后无法同时回到多窗口协作的实际问题。

技术上可行，但不能把它简化为保存 React state 或在启动时重新执行最近命令。可靠实现的前提是：持久化边界清晰、原生会话身份可验证、恢复有明确人工确认、旧协调任务彻底隔离。

下一步是执行 Phase 4：用受控假 CLI fixture 覆盖 2/3/4 Pane 恢复路径，并在安装了目标 CLI 的机器上验证关闭、强制退出、重启、逐 Pane 失败和重复恢复。未通过身份验证的工具仍可恢复工作区布局，但不得宣称其对话会被自动且正确地恢复。
