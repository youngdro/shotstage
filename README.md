# shotstage

用 3D 白模预演镜头，给 AI 生图 / 生视频当参考。Rust + wgpu + egui，同一份代码跑在浏览器（WebAssembly）和桌面端。所有编辑操作都是命令，**网页 JS、CLI、MCP 三种入口调用的是同一张命令表**，agent 能做的和人在界面上能做的完全一样。

![编辑器](docs/images/editor.png)

| 透过镜头看（干净的参考画面） | 深度 pass |
| --- | --- |
| ![look through](docs/images/look-through.png) | ![depth](docs/images/depth-pass.png) |

> 状态：M0 + 积木式搭景。场景、网格与修改器、相机、时间线关键帧、命令系统、CLI、MCP、可嵌入的网页编辑器、拖拽摆放、真实尺寸预制件、按景别构图、截图已经可用；离屏导出（mp4 / 控制图序列）、glTF 导入、可摆姿态的人偶在后续里程碑。

## 像搭积木一样搭场景

- **从左侧素材栏拖进视口**：物体落在鼠标下的表面上，可能是地面、桌面或别的物体顶上；拖出视口松手或按 Esc 就取消。单击素材则放到画面中间。
- **真实尺寸预制件**：人偶 1.72 m、餐桌、椅子、沙发、床、楼梯、墙、门洞、车、树、落地灯、箱子。它们都是一个组，整组一起拖动；在已选中的组里再点一次，可以选中单个部件。
- **直接拖动场景里的物体**：物体沿表面滑动，并且始终停在鼠标下面。**Shift** 竖直抬高；**Alt** 拖动时复制整组；拖动中滚动滚轮或按 **R** 旋转 15°（Shift+R 反向）；默认按 0.25 m 吸附网格，按住 **Ctrl** 可自由摆放；**Esc** 撤回这次拖动。拖动时会显示落点的投影。
- **专业手柄**：选中物体后出现 X/Y/Z 移动轴、水平旋转环和等比缩放方块，可以精确调整。
- **锁定**：地面、背景这类物体可以勾选 Locked，在它们上面拖动会旋转视角，不会把它们拖走。
- **景别构图**：选中主体后，在右侧 "Frame it" 选角度（平视、俯、仰、鸟瞰、虫视）和方位（正面、侧面、背面等），再点 ECU / CU / MCU / MS / MFS / FS / WS / EWS，镜头就会自动就位，并切换到镜头视角。

![Frame it：点 MS 后镜头自动构成中景](docs/images/frame-it.png)

每一次拖动只算一步撤销，背后调用的是 `entity.place`、`entity.moveTo`、`entity.rotate`、`prefab.create`、`camera.frame` 这些命令，agent 可以用完全相同的方式操作。

## 结构

| crate | 作用 | 依赖 GPU / 浏览器 |
| --- | --- | --- |
| `crates/core` | 场景文档、可编辑网格（n-gon）、参数化几何体、修改器栈、相机模型、时间线求值、镜头描述 | 否 |
| `crates/commands` | 命令注册表、JSON Schema、撤销/重做、原子批量、拖拽合并撤销 | 否 |
| `crates/render` | wgpu 渲染器：粘土 / 深度 / 法线 pass，网格、相机视锥、拾取 | GPU |
| `crates/app` | egui 编辑器；wasm 导出 `mount(container, options)` | GPU / 浏览器 |
| `crates/cli` | `shotstage` 命令行：`run` / `batch` / `serve` / `mcp` | 否 |

## 在网页里嵌入

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130   # 与 Cargo.lock 中的 wasm-bindgen 版本一致
./scripts/build-web.sh                             # 产出 dist/pkg/shotstage_app.js + _bg.wasm
python3 -m http.server -d dist 8080                # 打开 http://localhost:8080
```

编辑器挂到任意 DOM 容器里，并填满这个容器，跟随它的尺寸变化。同一页面可以挂多个实例，每个实例的场景和历史互相独立：

```js
import init, { mount } from "./pkg/shotstage_app.js";
await init();

const editor = await mount("#stage", { scene: savedSceneJson }); // 元素或 CSS 选择器；scene 可选
window.shotstage = editor;                                        // 可选：挂到全局给 agent / 控制台用

editor.dispatch("entity.create", { type: "mesh", primitive: { shape: "cube", size: [1, 1, 1] } });
editor.batch([                                                    // 一个事务：全部成功或全部回滚，一步撤销
  { command: "entity.create", params: { type: "camera", id: "cam", position: [0, 1.6, 5] } },
  { command: "camera.lookAt", params: { id: "cam", target: [0, 1, 0] } },
  { command: "camera.dollyMove", params: { id: "cam", distance: 2, start: 0, end: 48 } },
]);
editor.request({ command: "entity.translate", params: { id: "cube_1", delta: [0.1, 0, 0] }, merge: "drag-7" }); // 同一 merge key 合并成一步撤销
editor.commands();                 // 全部命令 + 参数 JSON Schema
editor.scene();                    // 场景文档（保存它，下次 mount 时传回）
const sub = editor.on(ev => {});   // { type: "change" | "command", revision, request }
editor.off(sub);
const shot = await editor.snapshot({ pass: "depth", width: 1024 }); // 透过镜头截图：{ dataUrl, data(base64), width, height, ... }
editor.destroy();                  // 停止渲染并移除 canvas
```

失败时 `dispatch` 会抛出 `Error`，`error.code` 取值为 `invalid_params`、`unknown_command`、`failed`、`nothing_to_undo` 等。除了文档命令，网页实例还多了四个编辑器命令：`editor.select`、`editor.setFrame`、`editor.setView`（`pass: clay|depth|normal`、`lookThrough`）和 `editor.state`。

浏览器有 WebGPU 时用 WebGPU，没有时自动回退到 WebGL2（已在 SwiftShader WebGL2 下验证）。

## 命令行与 MCP

```sh
cargo build --release -p shotstage-cli
shotstage commands --brief                       # 命令目录
shotstage commands                               # 带 JSON Schema 的完整目录
shotstage run entity.create '{"type":"mesh","primitive":{"shape":"sphere","radius":0.5,"segments":24,"rings":12}}' -s shot.json
shotstage batch steps.json -s shot.json          # [{"command": ..., "params": ...}, ...]
shotstage serve -s shot.json                     # 常驻会话：stdin/stdout 每行一个 JSON 请求
shotstage mcp -s shot.json                       # MCP（stdio），改动自动写回 shot.json
shotstage snapshot -s shot.json -o frame.png --pass depth   # 透过镜头渲染 PNG（需要 GPU 或软件 Vulkan，如 lavapipe）
```

`serve` 的每行请求格式为 `{"id": 1, "command": "...", "params": {...}}` 或 `{"id": 2, "batch": [...]}`，回复会带回同一个 `id`。

接到 Claude Code：

```sh
claude mcp add shotstage -- /path/to/shotstage mcp --scene /abs/path/shot.json
```

MCP 服务把每条命令都发布成一个工具（`entity.create` 对应 `entity_create`，入参就是该命令的 JSON Schema），另外提供 `command_list`、`command_run`、`command_batch`、`scene_save` 四个通用工具。`render_snapshot` 以 MCP 图片内容返回，模型可以直接看到画面，再根据画面修正构图。

## 命令目录

| 分组 | 命令 |
| --- | --- |
| 会话 | `command.list` `history.undo` `history.redo` `history.info` |
| 场景 | `scene.inspect` `scene.summary` `scene.new` `scene.load` `scene.evaluate` `scene.raycast` |
| 积木摆放 | `prefab.create` `prefab.list` `entity.place`（`ray` 落到射线击中处 / `on` 放到某物顶上 / `nextTo`+`side`(+`view` 按镜头左右) 并排 / 不带参数时落到正下方）`entity.moveTo` `entity.rotate` `entity.face` `entity.bounds` |
| 实体 | `entity.create` `entity.delete` `entity.duplicate`（连同子物体） `entity.rename` `entity.get` `entity.setTransform` `entity.translate` `entity.setParent` `entity.setVisible` `entity.setLocked` |
| 网格 | `mesh.setPrimitive` `mesh.setData` `mesh.setColor` `mesh.addModifier` `mesh.removeModifier` `mesh.applyModifiers` `mesh.join` `mesh.setOriginToCenter` `mesh.stats` `mesh.exportObj` |
| 相机 | `camera.frame`（景别 + 角度 + 方位自动构图） `camera.setLens` `camera.lookAt` `camera.setActive` `camera.describe` `camera.trajectory` `camera.orbitMove` `camera.dollyMove` |
| 渲染 | `render.snapshot`（clay / depth / normal，返回 PNG；由编辑器、CLI、MCP 执行） |
| 时间线 | `timeline.set` `keyframe.set` `keyframe.delete` `keyframe.list` `shot.create` `shot.delete` |

约定：长度单位是米，角度单位是度，时间单位是帧；+Y 朝上，地面是 y = 0，物体正面朝 +Z，相机朝 -Z 看。`camera.frame` 生成的镜头再用 `camera.describe` 描述时，得到的是同一个景别。`camera.trajectory` 输出的是 OpenCV 坐标轴下的逐帧内参和 3×4 世界到相机矩阵（RealEstate10K 风格），可以直接喂给相机控制类视频模型。`camera.describe` 用影视术语描述镜头，例如 `medium shot, low angle, 35mm normal lens`。

参数一律当作不可信输入处理：缺字段、类型错误、越界或未知字段都会返回错误，场景保持不变。

## 开发

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo clippy -p shotstage-app --target wasm32-unknown-unknown
cargo run -p shotstage-app [scene.json]          # 原生编辑器
```

## 路线图

- **M1 场景编辑**：glTF/GLB 导入、关节式人偶与姿态预设、按名字引用物体、防穿插、更多网格命令
- **M2 镜头与时间线**：时间线 UI（关键帧拖拽、曲线）、相机支架（轨道 / 摇臂）、更多命名运镜
- **M3 导出**：离屏逐帧渲染，导出首尾帧、mp4、深度 / 法线 / OpenPose / ID 序列，以及导出包
- **M4 Agent**：`render_frame` 返回图片、MCP 实时桥（agent 驱动用户打开的网页编辑器）、权限分级
- **二期网格编辑**：点 / 边 / 面编辑模式，挤出、倒角、循环切割、布尔运算、减面

## License

MIT OR Apache-2.0
