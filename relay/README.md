# shotstage relay

让 Claude（或任何 MCP 客户端）直接操控用户在浏览器里打开的在线编辑器，用户电脑上什么都不用装。

```
浏览器里的编辑器 ──wss──▶ /page/<code> ┐
                                        ├─ 每个连接码一个 Durable Object，只转发消息
Claude（MCP）     ──https─▶ /mcp/<code>  ┘
```

- 编辑器顶栏点 **🤖 连接 Claude**，页面生成一个私密连接码（130 位随机数，记在这台浏览器里），连上 `/page/<code>`，并显示 MCP 地址 `https://<relay>/mcp/<code>`。
- 把这个地址加到 Claude：claude.ai / Claude 桌面版在 **设置 → 连接器 → 添加自定义连接器**；Claude Code 用 `claude mcp add --transport http shotstage <地址>`。
- 中继自己回答 `initialize`、`tools/list`（定义在 `web/agent-tools.js`，页面和中继共用），每个 `tools/call` 交给页面，在编辑器里通过 `editor.request` / `editor.call` 执行（`web/agent.js`），结果原样返回。所以改动实时出现在页面上、进撤销历史，截图和分镜由浏览器渲染并以图片返回给 Claude。
- 工具：`editor_status`、`command_list`、`command_run`、`command_batch`、`render_snapshot`、`render_storyboard`，和 `shotstage mcp --compact` 一样，命令表里的每条命令都能通过 `command_run` 调用；也接受 `camera_frame` 这类按命令名发布的工具名。
- 连接码就是密钥：有地址的人都能改这个页面里的场景。面板里 **换一个新地址** 让旧地址立刻失效。同一个地址在新标签页打开时，新标签页接管，旧的停止。
- 不存任何东西，只在两端都在线时转发消息。只接受 `ALLOWED_ORIGINS`（`wrangler.toml`）里的网页和 localhost 连接页面端。

## 部署（Cloudflare Workers 免费版即可）

一次性准备：

1. 注册 Cloudflare（免费），在控制台打开一次 **Workers & Pages**，选一个 `workers.dev` 子域名。
2. **My Profile → API Tokens → Create Token**，用 **Edit Cloudflare Workers** 模板生成令牌；在控制台首页右侧复制 **Account ID**。
3. GitHub 仓库 **Settings → Secrets and variables → Actions** 新建两个 secret：`CLOUDFLARE_API_TOKEN`、`CLOUDFLARE_ACCOUNT_ID`。

之后 main 上 `relay/` 有改动时，`.github/workflows/relay.yml` 自动测试并部署（也可在 Actions 里手动运行 **Relay**），日志里会打印地址 `https://shotstage-relay.<子域名>.workers.dev`。把它设为仓库变量 `SHOTSTAGE_RELAY`（同一页面的 **Variables**），网页构建（`pages.yml` → `scripts/build-web.sh`）就会写入 `config.json`，顶栏出现 **连接 Claude** 按钮。没有设置时不显示按钮。

本地部署也行：`cd relay && npm ci && npx wrangler login && npx wrangler deploy`。

## 开发

```bash
cd relay && npm ci
npm test                        # 在本地 workerd 里跑中继，用假页面和 MCP 请求端到端测试
npx wrangler dev                # 本地中继 http://127.0.0.1:8787
SHOTSTAGE_RELAY=http://127.0.0.1:8787 ../scripts/build-web.sh   # 或打开页面时加 ?relay=http://127.0.0.1:8787
```
