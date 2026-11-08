// What an MCP client sees when it drives the online editor through the relay (relay/):
// the server instructions and the tool list. Plain data, shared by the relay (which answers
// `initialize` and `tools/list` even while no page is open) and the page (web/agent.js, which
// runs the tool calls). The tools are the generic ones of `shotstage mcp --compact`; every
// command of the catalog is reachable through `command_run`.

export const SERVER_NAME = "shotstage";

export const INSTRUCTIONS = `\
shotstage is a 3D previs stage running in the person's browser: block out a scene with real-size \
stand-ins, place cameras by shot size, animate, and export reference frames for AI image / video \
models. Every change you make appears live in their editor and lands in its undo history.

Tools: command_list (every command with its parameter JSON Schema; filter by prefix such as \
"camera."), command_run {command, params} for any one command, command_batch for several as one \
undo step, render_snapshot to see the shot (you get the image back), render_storyboard for every \
shot's frames, editor_status to check that the page is connected. Command ids below are run with \
command_run, e.g. {command: "scene.template", params: {...}}.

Workflow that works well:
1. Start: scene.template (scene.templates lists them) for a ready set, or scene.new and build up.
2. Set: prefab.create (prefab.list: people, fresh-food retail fixtures and produce, product-shoot \
props and backdrops, furniture), entity.place (rest on / next to / drop), entity.face, \
entity.attach (props in hand).
3. People: pose.set (pose.list), pose.aim, person.lookAt; realistic people also take \
person.setBody / person.setOutfit / person.setExpression when available.
4. Light: light.preset (threePoint, loop, clamshell, productSoftbox, highKey, freshCounter, lowKey, \
neon, goldenHour, daylight, overcast...); environment.set picks a sky; light.shadows tunes shadows.
5. Camera: camera.frame {subject, size: closeUp|medium|full|wide..., angle, side} places the \
camera for you; check it with render_snapshot and camera.describe.
6. Motion: motion.apply {preset, target, subject} (motion.list: push-ins, orbits, crane, \
handheld, product spins, character actions); shot.create / shot.cut lay out the edit.
7. Deliver: render_storyboard (first / middle / last frame of each shot).

Read the scene with scene.summary, entity.bounds and editor.state. Every change is one undo step \
(history.undo). Errors say what is wrong and leave the scene unchanged. Commands that read or \
write files (model.import with a path, scene.save to a path...) are not available in the browser: \
the person can import files themselves. Units: metres, degrees, frames; +Y up, ground y = 0, \
object fronts face +Z, cameras look down -Z.`;

const STEP = {
  type: "object",
  required: ["command"],
  properties: {
    command: { type: "string", description: "Command id, e.g. `prefab.create`" },
    params: { type: "object", description: "The command's params (see command_list)" },
  },
};

export const TOOLS = [
  {
    name: "editor_status",
    description: "Whether the person's shotstage editor is open and connected to this link, the address to open, and (when connected) a short summary of the scene.",
    inputSchema: { type: "object", properties: {} },
    annotations: { readOnlyHint: true },
  },
  {
    name: "command_list",
    description: "List every shotstage command with its parameter JSON Schema.",
    inputSchema: { type: "object", properties: { filter: { type: "string", description: "Only ids starting with this prefix, e.g. `camera.`" } } },
    annotations: { readOnlyHint: true },
  },
  {
    name: "command_run",
    description: "Run any shotstage command by id with JSON params (see command_list). The change appears in the person's editor as one undo step.",
    inputSchema: STEP,
  },
  {
    name: "command_batch",
    description: "Run several commands as one transaction (all or nothing, one undo step).",
    inputSchema: { type: "object", required: ["steps"], properties: { steps: { type: "array", items: STEP } } },
  },
  {
    name: "render_snapshot",
    description: "Render the view through a camera and return the picture, so you can check the framing.",
    inputSchema: {
      type: "object",
      properties: {
        camera: { type: "string", description: "Camera to look through. Default: the camera in use at `frame`." },
        frame: { type: "number", description: "Default: the editor's current frame." },
        pass: { type: "string", enum: ["clay", "depth", "normal"], description: "Default clay." },
        width: { type: "integer", description: "Pixels, 16..=4096. Default 1024 wide; the other side follows the camera's aspect." },
        height: { type: "integer" },
      },
    },
    annotations: { readOnlyHint: true },
  },
  {
    name: "render_storyboard",
    description: "Render the storyboard: first / middle / last frame of each shot, returned as pictures with the shot list (same params as the `render.storyboard` command, without `out`).",
    inputSchema: { type: "object", properties: {}, additionalProperties: true },
    annotations: { readOnlyHint: true },
  },
];
