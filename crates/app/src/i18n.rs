//! Interface language: English or Chinese, chosen from the system / browser locale and
//! remembered. Texts are looked up by their English wording (`tr`), formatted messages pick
//! one of two format strings (`lf!`). Command names and the API stay in English.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static ZH: AtomicBool = AtomicBool::new(false);

/// Is the interface in Chinese?
pub fn zh() -> bool {
    ZH.load(Ordering::Relaxed)
}

/// Switch the language and remember the choice.
pub fn set_zh(on: bool) {
    ZH.store(on, Ordering::Relaxed);
    save_choice(if on { "zh" } else { "en" });
}

/// Pick the language at start: the remembered choice, else the system / browser locale.
pub fn init() {
    let on = match load_choice().as_deref() {
        Some("zh") => true,
        Some("en") => false,
        _ => system_locale().is_some_and(|l| l.to_ascii_lowercase().starts_with("zh")),
    };
    ZH.store(on, Ordering::Relaxed);
}

/// `text` in the interface language (unchanged when there is no translation).
pub fn tr(text: impl AsRef<str>) -> String {
    let s = text.as_ref();
    if !zh() {
        return s.to_string();
    }
    table().get(s).map(|t| t.to_string()).unwrap_or_else(|| s.to_string())
}

/// Is there a Chinese wording of `text`?
#[cfg(test)]
pub fn has(text: &str) -> bool {
    table().contains_key(text)
}

/// Both wordings of `text`, for searching in either language.
pub fn both(text: &str) -> String {
    match table().get(text) {
        Some(t) => format!("{text} {t}"),
        None => text.to_string(),
    }
}

/// A formatted message in the interface language: `lf!("Saved: {w}", "已保存：{w}")`.
#[macro_export]
macro_rules! lf {
    ($en:literal, $zh:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::zh() { format!($zh $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| ZH_TABLE.iter().copied().collect())
}

#[cfg(not(target_arch = "wasm32"))]
fn settings_file() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".shotstage").join("language"))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_choice() -> Option<String> {
    std::fs::read_to_string(settings_file()?).ok().map(|s| s.trim().to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn save_choice(lang: &str) {
    if let Some(f) = settings_file() {
        let _ = f.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(f, lang);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn system_locale() -> Option<String> {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
            && !v.starts_with('C')
            && !v.starts_with("en_US.UTF")
        {
            return Some(v);
        }
    }
    // macOS keeps the interface language out of the environment
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = s.split('"').nth(1) {
            return Some(first.to_string());
        }
    }
    std::env::var("LANG").ok()
}

#[cfg(target_arch = "wasm32")]
fn load_choice() -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item("shotstage.lang").ok()?
}

#[cfg(target_arch = "wasm32")]
fn save_choice(lang: &str) {
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item("shotstage.lang", lang);
    }
}

#[cfg(target_arch = "wasm32")]
fn system_locale() -> Option<String> {
    web_sys::window()?.navigator().language()
}

/// Chinese glyphs: a subset of Noto Sans SC (SIL OFL) with the characters the interface uses
/// plus the most common ones, added after the default fonts so Latin text keeps its look. On
/// the desktop a system Chinese font, when there is one, covers everything else people type.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto_sc".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../../assets/fonts/NotoSansSC-subset.otf"))),
    );
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut extra = Vec::from(["noto_sc".to_string()]);
    #[cfg(not(target_arch = "wasm32"))]
    for (path, index) in [
        ("/System/Library/Fonts/PingFang.ttc", 0),
        ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
        ("/System/Library/Fonts/STHeiti Light.ttc", 0),
        ("C:\\Windows\\Fonts\\msyh.ttc", 0),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
        ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            let mut data = egui::FontData::from_owned(bytes);
            data.index = index;
            fonts.font_data.insert("system_cjk".into(), std::sync::Arc::new(data));
            extra.push("system_cjk".into());
            break;
        }
    }
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&family) {
            list.extend(extra.iter().cloned());
        }
    }
    ctx.set_fonts(fonts);
}

/// The help text, in the interface language.
pub fn help(en: &'static str) -> &'static str {
    if zh() { HELP_ZH } else { en }
}

const HELP_ZH: &str = "快捷操作：Cmd/Ctrl+K 或 / 打开搜索框，可以搜到所有素材、运动、姿势、模板和操作。\n\
素材：从左侧面板拖进视图（或单击放到视图中央）。\n\
物体：单击选中（再点一次选中部件）；拖动可在表面上滑动。\n\
Shift+拖动：抬起 · Alt+拖动：复制 · R / Shift+R 或拖动时滚轮：旋转 · Ctrl：不吸附 · Esc：取消。\n\
人物：点身体某处选中关节，然后拖动它弯曲，或转动彩色圆环。\n\
视图：拖空白处旋转 · 右键拖或 Shift+拖平移 · 捏合或鼠标滚轮缩放 · 触控板双指滑动旋转（Shift：平移）· 双击或 F 聚焦。\n\
按键：Cmd/Ctrl+Z 撤销 · Cmd/Ctrl+Shift+Z 重做 · Cmd/Ctrl+C / X / V 复制、剪切、粘贴（也能跨场景）· Cmd/Ctrl+D 复制一份 · Delete 删除 · 空格 播放 · K 关键帧 · F 聚焦 · Cmd/Ctrl+S 保存。\n\
多个物体：Shift+单击 加选 · 在空白处 Cmd/Ctrl+拖动 框选 · Cmd/Ctrl+G 编组 · Cmd/Ctrl+Shift+G 解组。";

/// English wording → Chinese.
static ZH_TABLE: &[(&str, &str)] = &[
    // top bar, toolbars, menus
    ("Open…", "打开…"),
    ("Save", "保存"),
    ("New", "新建"),
    ("Undo", "撤销"),
    ("Redo", "重做"),
    ("History", "历史"),
    ("Import…", "导入…"),
    ("Templates", "模板"),
    ("Export", "导出"),
    ("🔍 Actions", "🔍 操作"),
    ("3D previs for AI images and video", "面向 AI 图像和视频的 3D 预演"),
    ("Clay", "白模"),
    ("Depth", "深度"),
    ("Normal", "法线"),
    ("Clay shading", "白模着色"),
    ("Depth pass (near = white)", "深度通道（近处为白）"),
    ("Normal pass", "法线通道"),
    ("Snap", "吸附"),
    ("Snap dragged blocks to a grid (hold Ctrl to place freely)", "拖动时吸附到网格（按住 Ctrl 自由摆放）"),
    ("🎥 Camera view", "🎥 相机视角"),
    ("Look through the shot camera (the clean reference frame)", "透过镜头相机看（干净的参考画面）"),
    ("📷 Save frame", "📷 存当前帧"),
    (
        "Save the shot camera's view at this frame as a PNG (1920 px wide)",
        "把这一帧镜头相机的画面存成 PNG（1920 像素宽）",
    ),
    ("+ Camera", "+ 相机"),
    ("A new camera right where you are looking from", "在你当前的视角放一个新相机"),
    ("View ▾", "视图 ▾"),
    ("Shadows", "阴影"),
    ("Studio light", "影棚光"),
    (
        "Show the flat studio clay light instead of the scene's lights (scenes without lights always use it)",
        "用平整的影棚白模光代替场景灯光（没有灯的场景总是用它）",
    ),
    ("Shadows from the key light (snapshots always have them)", "主光的阴影（快照总是带阴影）"),
    ("Safe areas in camera view", "相机视角里显示安全框"),
    (
        "Action / title safe frames and a centre cross when looking through a camera",
        "透过相机看时显示动作 / 字幕安全框和中心十字",
    ),
    ("Camera preview in the corner", "角落里的镜头小窗"),
    (
        "A small live view of the shot camera while you work in the free view",
        "在自由视角工作时，实时显示镜头相机画面的小窗",
    ),
    ("Two-finger scroll / mouse wheel", "双指滑动 / 鼠标滚轮"),
    ("Auto", "自动"),
    ("Orbit", "旋转"),
    ("Zoom", "缩放"),
    ("Trackpad orbits, mouse wheel zooms", "触控板旋转，鼠标滚轮缩放"),
    ("Scrolling orbits (Shift: pan); pinch or Ctrl+scroll zooms", "滚动旋转视角（Shift：平移）；捏合或 Ctrl+滚动缩放"),
    ("Scrolling always zooms", "滚动总是缩放"),
    ("Shadows, lighting, camera preview and navigation", "阴影、灯光、镜头小窗和导航"),
    ("Open a saved scene (.json), or import models and pictures", "打开保存的场景（.json），或导入模型和图片"),
    ("Save the scene as a .shotstage.json file (Cmd/Ctrl+S)", "把场景存成 .shotstage.json 文件（Cmd/Ctrl+S）"),
    ("Clear the scene (undoable)", "清空场景（可撤销）"),
    (
        "Search every block, move, pose, template and action (Cmd/Ctrl+K or /)",
        "搜索所有素材、运动、姿势、模板和操作（Cmd/Ctrl+K 或 /）",
    ),
    ("Click a step to go back to before it", "点某一步回到它之前"),
    ("Nothing to undo yet", "还没有可撤销的操作"),
    ("Start from a ready scene (replaces this one; Undo brings it back)", "从现成场景开始（替换当前场景，撤销可恢复）"),
    ("The scene posed at the current frame", "当前帧姿态下的场景"),
    (
        "glTF / GLB, OBJ, STL, FBX and .blend (FBX / .blend on the desktop, through Blender). You can also drop files on the view.",
        "glTF / GLB、OBJ、STL、FBX 和 .blend（FBX / .blend 需桌面版通过 Blender）。也可以把文件拖到视图里。",
    ),
    ("Images", "图片"),
    (
        "Pick a picture, or drop one on the view (a transparent PNG becomes a cut-out).",
        "选一张图片，或拖到视图里（透明 PNG 会变成抠像）。",
    ),
    ("Export failed: no data", "导出失败：没有数据"),
    ("Language", "语言"),
    ("Interface language", "界面语言"),
    // export formats, image modes
    (
        "Meshes, colours, cameras and lights: Blender, Unreal, Unity, three.js",
        "网格、颜色、相机和灯光：Blender、Unreal、Unity、three.js",
    ),
    ("The same as GLB, as one JSON file", "和 GLB 相同，存成一个 JSON 文件"),
    ("Meshes and colours, for almost any 3D tool", "网格和颜色，几乎所有 3D 软件都能用"),
    ("Geometry only, for 3D printing and CAD", "只有几何体，用于 3D 打印和 CAD"),
    ("Needs Blender on this computer (desktop editor)", "需要这台电脑上有 Blender（桌面版）"),
    ("Card", "立牌"),
    ("Cut-out", "抠像"),
    ("Backdrop", "背景"),
    ("Floor", "地面"),
    ("A standing picture: reference photo, poster, sign", "竖立的图片：参考照片、海报、标牌"),
    ("Always faces the camera: cut-out people, trees", "总是朝向相机：抠像人物、树"),
    ("Fills the camera's background, behind everything", "铺满相机背景，在所有物体后面"),
    ("Lies flat: rug, floor plan, logo on the ground", "平铺：地毯、平面图、地上的标志"),
    // palette
    ("Blocks", "素材"),
    ("Search blocks…", "搜索素材…"),
    ("Drag into the scene, or click to drop it in the middle.", "拖进场景，或单击放到中间。"),
    ("People", "人物"),
    ("Fresh retail", "生鲜零售"),
    ("Product shoot", "产品拍摄"),
    ("Furniture & set", "家具与场景"),
    ("Shapes", "基本形状"),
    ("Lights", "灯光"),
    ("Lighting presets", "灯光预设"),
    ("Cameras & helpers", "相机与辅助"),
    (
        "Basic shape (edit its size in Mesh / with mesh.setPrimitive)",
        "基本形状（在「网格」里或用 mesh.setPrimitive 改尺寸）",
    ),
    ("Cube", "立方体"),
    ("Slab", "板"),
    ("Pillar", "柱子"),
    ("Sphere", "球"),
    ("Cylinder", "圆柱"),
    ("Cone", "圆锥"),
    ("Torus", "圆环"),
    ("Camera", "相机"),
    ("Empty", "空物体"),
    ("A camera where you are looking from", "在你当前的视角放一台相机"),
    ("A group / pivot without geometry", "没有几何体的组 / 枢轴"),
    ("Sun", "太阳光"),
    ("Point light", "点光"),
    ("Point", "指向"),
    ("Beam angle", "光束角"),
    ("Scales", "电子秤"),
    ("Spot", "聚光"),
    ("Area", "面光"),
    ("Parallel sunlight; aim it to set the time of day", "平行的太阳光；调方向来设定时间"),
    ("A bare bulb: light in every direction", "裸灯泡：向四周发光"),
    ("A cone of light (stage light, rim light)", "锥形光（舞台光、轮廓光）"),
    ("A soft panel: softbox, window", "柔光面板：柔光箱、窗户"),
    ("Three-point", "三点布光"),
    ("Rembrandt", "伦勃朗光"),
    ("Backlight", "逆光"),
    ("Golden hr", "黄金时刻"),
    ("Daylight", "日光"),
    ("No lights", "不要灯光"),
    ("Key, fill and rim around the subject", "主光、辅光和轮廓光围绕主体"),
    ("One high side key light, moody", "一盏高位侧光，氛围感强"),
    ("Rim light from behind", "从后面打的轮廓光"),
    ("Warm low sun from the side", "侧面低角度的暖色阳光"),
    ("High neutral sun", "高处的中性阳光"),
    ("Remove all lights: back to the flat studio light", "去掉所有灯光：回到平整的影棚光"),
    // scene list
    ("Scene", "场景"),
    ("Filter…", "筛选…"),
    ("Nothing matches.", "没有匹配的结果。"),
    (
        "Empty scene. Add blocks above, or drive it from the console / API.",
        "空场景。从上面添加素材，或用控制台 / API 驱动。",
    ),
    ("Focus", "聚焦"),
    ("Rename", "改名"),
    ("Duplicate", "复制一份"),
    ("Hide", "隐藏"),
    ("Show", "显示"),
    ("Lock", "锁定"),
    ("Unlock", "解锁"),
    ("Delete", "删除"),
    ("Unlock (drags pass through locked objects)", "解锁（拖动会穿过锁定的物体）"),
    ("Lock: drags pass through it", "锁定：拖动会穿过它"),
    // properties
    ("Nothing selected.", "没有选中任何东西。"),
    ("• Drag a block from the left into the scene.", "• 从左侧把素材拖进场景。"),
    ("• Click an object to select it; drag it to slide it over surfaces.", "• 单击物体选中它；拖动可在表面上滑动。"),
    ("• Click a person, then a body part, to pose it.", "• 先点人物，再点身体某处，就能摆姿势。"),
    (
        "• Drag empty space (or two fingers on a trackpad) to orbit; pinch or wheel to zoom.",
        "• 拖动空白处（或触控板双指）旋转视角；捏合或滚轮缩放。",
    ),
    ("Click to rename", "点击改名"),
    ("Visible", "可见"),
    ("Locked", "锁定"),
    ("Drags in the viewport pass through it", "视口里的拖动会穿过它"),
    ("Ctrl/Cmd+D", "Ctrl/Cmd+D"),
    ("joint", "关节"),
    ("person", "人物"),
    ("group", "组"),
    ("mesh", "网格"),
    ("camera", "相机"),
    ("light", "灯光"),
    ("image", "图片"),
    ("empty", "空物体"),
    ("Place", "摆放"),
    ("Drop to surface", "落到表面"),
    ("Hold in hand", "拿在手里"),
    ("Face camera", "面向相机"),
    ("It now follows the hand: pose or animate the person", "它现在跟着手走：给人物摆姿势或做动画"),
    ("Let go", "松手"),
    ("Stop following the hand (stays where it is)", "不再跟着手（留在原处）"),
    ("right hand", "右手"),
    ("left hand", "左手"),
    ("Frame it", "构图"),
    ("Moves the active camera (or adds one) and looks through it.", "移动活动相机（没有就新建一台）并透过它看。"),
    ("Angle", "角度"),
    ("Side", "方向"),
    ("eyeLevel", "平视"),
    ("high", "高角度"),
    ("low", "低角度"),
    ("birdsEye", "鸟瞰"),
    ("wormsEye", "仰拍"),
    ("front", "正面"),
    ("back", "背面"),
    ("left", "左侧"),
    ("right", "右侧"),
    ("frontLeft", "左前"),
    ("frontRight", "右前"),
    ("backLeft", "左后"),
    ("backRight", "右后"),
    ("ECU", "大特写"),
    ("CU", "特写"),
    ("MCU", "近景"),
    ("MS", "中景"),
    ("MFS", "中全景"),
    ("FS", "全景"),
    ("WS", "远景"),
    ("EWS", "大远景"),
    ("Animate", "动画"),
    ("Search moves…", "搜索运动…"),
    ("Length", "时长"),
    ("preset", "预设"),
    ("How long the move takes (preset = its own length)", "运动持续多久（预设 = 用它自己的时长）"),
    ("Strength", "强度"),
    (
        "Select an object first to move around it (otherwise: what the camera looks at).",
        "先选一个物体，就围绕它运动（否则围绕相机看着的地方）。",
    ),
    ("Transform", "变换"),
    ("Position", "位置"),
    ("Rotation", "旋转"),
    ("Scale", "缩放"),
    ("Mesh", "网格"),
    ("Colour", "颜色"),
    ("Size", "尺寸"),
    ("Origin to centre", "原点移到中心"),
    ("Apply all", "全部应用"),
    ("Pose", "姿势"),
    (
        "Click a body part to select its joint, then drag it (or the rings) to bend it.",
        "点身体某处选中关节，再拖它（或圆环）来弯曲。",
    ),
    ("Hair", "发型"),
    ("Look at camera", "看向相机"),
    ("Turn the head and neck towards the shot camera", "把头和脖子转向镜头相机"),
    ("Joint", "关节"),
    (
        "Drag a body part in the view to bend it, or turn the coloured rings.",
        "在视图里拖身体某处来弯曲，或转动彩色圆环。",
    ),
    ("Reset", "重置"),
    ("Select person", "选中人物"),
    ("Bone", "骨骼"),
    (
        "Drag the model's surface in the view to bend this bone, or turn the coloured rings.",
        "在视图里拖模型表面来弯曲这根骨骼，或转动彩色圆环。",
    ),
    ("Select model", "选中模型"),
    ("Bend  X", "弯曲 X"),
    ("Twist  Y", "扭转 Y"),
    ("Side  Z", "侧弯 Z"),
    ("Warm", "暖"),
    ("Neutral", "中性"),
    ("Cool", "冷"),
    ("eye level", "平视"),
    ("high angle", "高角度"),
    ("low angle", "低角度"),
    ("bird's-eye", "鸟瞰"),
    ("worm's-eye", "仰拍"),
    ("ultra-wide lens", "超广角镜头"),
    ("wide lens", "广角镜头"),
    ("normal lens", "标准镜头"),
    ("portrait lens", "人像镜头"),
    ("telephoto lens", "长焦镜头"),
    ("Drop to import (glb, gltf, obj, stl, fbx, blend)", "松开即可导入（glb、gltf、obj、stl、fbx、blend）"),
    ("Light", "灯光"),
    ("Kind", "类型"),
    ("Intensity", "强度"),
    ("Softness", "柔和度"),
    (
        "A light shines along its -Z. Scenes with lights are lit by them; toggle Studio light in the view toolbar to compare.",
        "灯光沿自身 -Z 方向照射。有灯光的场景由灯光照明；可在视图工具栏切换「影棚光」对比。",
    ),
    ("Aim at selection's group", "对准选中物体的组"),
    ("Aim at origin", "对准原点"),
    ("Image", "图片"),
    ("Always face the camera (billboard)", "总是朝向相机（公告板）"),
    ("For cut-out people, trees and signs", "用于抠像人物、树和标牌"),
    ("Height", "高度"),
    ("Frame", "画幅"),
    ("Landscape video", "横屏视频"),
    ("Vertical video: Reels, TikTok, Shorts", "竖屏视频：抖音、Reels、Shorts"),
    ("Square", "方形"),
    ("Portrait feed post", "竖版信息流帖子"),
    ("Classic", "经典"),
    ("Cinemascope", "宽银幕"),
    ("Match current view", "匹配当前视角"),
    ("Put this camera where you are looking from in the view", "把这台相机放到你在视图里的视角"),
    ("Safe areas", "安全框"),
    ("Action / title safe frames in the camera view", "相机视角里的动作 / 字幕安全框"),
    ("Roll", "滚转"),
    ("Tilt the horizon (a Dutch angle)", "倾斜地平线（荷兰角）"),
    ("Level", "放平"),
    ("Reference", "参考图"),
    ("Load picture…", "载入图片…"),
    (
        "Lay a storyboard frame or a reference photo over this camera's view to match it",
        "把分镜或参考照片叠在这台相机的画面上来对齐",
    ),
    ("Remove the reference", "移除参考图"),
    ("Focal length", "焦距"),
    ("Arrange", "排列"),
    ("Group", "编组"),
    ("Delete all", "全部删除"),
    (
        "Shift+click adds or removes objects; click one alone to select just it.",
        "Shift+单击 加选或减选；单独点一个就只选它。",
    ),
    ("Align X", "X 对齐"),
    ("Align Y", "Y 对齐"),
    ("Align Z", "Z 对齐"),
    ("min", "最小"),
    ("centre", "居中"),
    ("max", "最大"),
    ("Distribute", "均匀分布"),
    ("Grouped: they now move together (Ctrl+Shift+G to ungroup)", "已编组：它们现在一起移动（Ctrl+Shift+G 解组）"),
    ("Ungrouped", "已解组"),
    ("Ungroup", "解组"),
    ("Animated: edits are keyed at the current frame.", "有动画：修改会在当前帧打关键帧。"),
    // timeline
    ("To the start (Home)", "到开头（Home）"),
    ("To the end (End)", "到结尾（End）"),
    ("▶ Play", "▶ 播放"),
    ("⏸ Pause", "⏸ 暂停"),
    ("Play", "播放"),
    ("Pause", "暂停"),
    ("Preview shot", "预览镜头"),
    ("🎥 Preview shot", "🎥 预览镜头"),
    ("model", "模型"),
    ("Play from the start, looking through the shot camera", "从头播放，透过镜头相机看"),
    ("◆ Key", "◆ 关键帧"),
    ("Key", "关键帧"),
    ("Keyframe the selection at this frame (K)", "在这一帧为选中物体打关键帧（K）"),
    ("Auto-key", "自动关键帧"),
    ("Every move, turn or pose change is keyed at the current frame", "每次移动、旋转或改姿势都在当前帧打关键帧"),
    ("+ Shot", "+ 镜头"),
    ("Fit", "全部显示"),
    (
        "Show the whole timeline (or double-click the ruler; pinch or Cmd/Ctrl+scroll on it to zoom)",
        "显示整条时间轴（或双击时间尺；在上面捏合或 Cmd/Ctrl+滚动可缩放）",
    ),
    ("Storyboard", "故事板"),
    ("Every shot as a picture through its camera; click one to go there", "每个镜头透过它的相机拍一张图；点一张跳过去"),
    ("Console", "控制台"),
    (
        "Type commands: the same API as window.shotstage, the CLI and MCP",
        "输入命令：和 window.shotstage、命令行、MCP 是同一套 API",
    ),
    ("Timeline", "时间轴"),
    ("Shots", "镜头"),
    ("No shots: + Shot (above) or right-click here", "还没有镜头：用上面的 + 镜头，或在这里右键"),
    (
        "No camera yet: + Camera in the view toolbar puts one where you look from",
        "还没有相机：视图工具栏的 + 相机 会在你当前的视角放一台",
    ),
    (
        "No shots yet: + Shot makes one through the selected or active camera",
        "还没有镜头：+ 镜头 会用选中的或活动的相机建一个",
    ),
    (
        "No shots yet: add one with + Shot, or right-click the Shots row on the timeline.",
        "还没有镜头：用 + 镜头 添加，或在时间轴的镜头轨上右键。",
    ),
    (
        "No animation yet: select something and press ◆ Key, or use a preset under Animate on the right.",
        "还没有动画：选中物体后按 ◆ 关键帧，或用右侧「动画」里的预设。",
    ),
    ("Look through its camera", "透过它的相机看"),
    ("Camera", "相机"),
    ("Split at the playhead", "在播放头处拆分"),
    ("Delete shot", "删除镜头"),
    ("Add a camera first", "请先添加相机"),
    ("Ease into the next key", "到下一个关键帧的缓动"),
    ("Smooth (ease)", "平滑（缓入缓出）"),
    ("Ease in", "缓入"),
    ("Ease out", "缓出"),
    ("Linear", "线性"),
    ("Overshoot", "回弹"),
    ("Hold (step)", "保持（阶跃）"),
    ("Delete key", "删除关键帧"),
    ("Start at the playhead", "从播放头开始"),
    ("Slide every key so the animation begins here", "平移所有关键帧，让动画从这里开始"),
    ("Faster", "变快"),
    ("Play it in two thirds of the time", "用三分之二的时间播完"),
    ("Slower", "变慢"),
    ("Take half as long again", "多花一半时间"),
    ("Tip: drag the bar between the keys to slide it all.", "提示：拖动关键帧之间的横条可整段平移。"),
    ("Command console", "命令控制台"),
    (
        "JSON request: {\"command\", \"params\"} or {\"batch\": [...]}",
        "JSON 请求：{\"command\", \"params\"} 或 {\"batch\": [...]}",
    ),
    ("List commands", "列出命令"),
    // storyboard
    ("Export…", "导出…"),
    (
        "Save every shot's picture as one HTML page (open it, print it or send it)",
        "把每个镜头的图片存成一个 HTML 页面（可打开、打印或发送）",
    ),
    ("To move it, change its lens or animate it", "用来移动它、换镜头或做动画"),
    ("Go to its middle frame", "跳到它的中间帧"),
    ("The frame this picture shows", "这张图显示的那一帧"),
    // viewport
    ("Pick up where you left off?", "从上次的地方继续？"),
    ("Restore it", "恢复"),
    ("Start fresh", "重新开始"),
    ("Restored your last scene (Undo goes back)", "已恢复上次的场景（撤销可回退）"),
    ("Start a scene", "开始一个场景"),
    (
        "Pick a ready set (everything stays editable), or drag blocks in from the left.",
        "选一个现成的场景（全部可编辑），或从左侧拖入素材。",
    ),
    (
        "Tip: drop a .glb / .fbx model or a picture on the view to bring it in. Cmd/Ctrl+K searches every block, move and action.",
        "提示：把 .glb / .fbx 模型或图片拖到视图里即可导入。Cmd/Ctrl+K 可搜索所有素材、运动和操作。",
    ),
    (
        "Click an axis to look along it (Y: from above) · 3/4: the default view",
        "点坐标轴沿它看（Y：从上往下）· 3/4：默认视角",
    ),
    ("Shot camera at the playhead: click to look through it", "播放头处的镜头相机：点击透过它看"),
    ("Pasted (Undo takes it back)", "已粘贴（撤销可撤回）"),
    ("Copied", "已复制"),
    ("Cut", "切镜头"),
    // quick actions
    ("Add a block, apply a move or pose, run an action…", "添加素材、应用运动或姿势、执行操作…"),
    ("↑↓ choose · Enter run · Esc close", "↑↓ 选择 · 回车执行 · Esc 关闭"),
    ("Action", "操作"),
    ("Add", "添加"),
    ("Lighting", "灯光"),
    ("Template", "模板"),
    ("Motion", "运动"),
    ("Camera move", "运镜"),
    ("Shot", "镜头"),
    ("Select", "选中"),
    ("Save scene", "保存场景"),
    ("Open / import a file…", "打开 / 导入文件…"),
    ("scene, model or picture", "场景、模型或图片"),
    ("New scene", "新建场景"),
    ("clear everything (undoable)", "清空全部（可撤销）"),
    ("Toggle shadows", "切换阴影"),
    ("New camera from this view", "从当前视角建相机"),
    ("where you are looking from", "放在你当前的视角"),
    ("Save frame as PNG", "把当前帧存成 PNG"),
    ("the shot camera at the playhead, 1920 px", "播放头处的镜头相机，1920 像素"),
    ("Export storyboard", "导出故事板"),
    ("every shot as one HTML page", "所有镜头存成一个 HTML 页面"),
    ("Play / pause", "播放 / 暂停"),
    ("Go to the start", "回到开头"),
    ("Toggle camera view", "切换相机视角"),
    ("look through the shot camera", "透过镜头相机看"),
    ("Toggle storyboard", "切换故事板"),
    ("every shot as a picture", "每个镜头一张图"),
    ("Toggle camera preview", "切换镜头小窗"),
    ("the small view in the corner", "角落里的小窗"),
    ("Fit the timeline", "显示整条时间轴"),
    ("show all frames", "显示所有帧"),
    ("Toggle safe areas", "切换安全框"),
    ("action / title safe frames in camera view", "相机视角里的动作 / 字幕安全框"),
    ("Focus the selection", "聚焦选中物体"),
    ("Key the selection", "为选中物体打关键帧"),
    ("K: keyframe at the playhead", "K：在播放头处打关键帧"),
    ("makes it the active camera", "设为活动相机"),
    ("Space", "空格"),
    // history labels
    ("Move", "移动"),
    ("Turn", "旋转"),
    ("Edit transform", "编辑变换"),
    ("Add block", "添加素材"),
    ("Retime key", "调整关键帧时间"),
    ("Retime animation", "调整动画时间"),
    ("Paste", "粘贴"),
    ("Frame shot", "构图"),
    ("Import model", "导入模型"),
    ("Add picture", "添加图片"),
    ("Lighting preset", "灯光预设"),
    ("Easing", "缓动"),
    ("Show / hide", "显示 / 隐藏"),
    ("Lock / unlock", "锁定 / 解锁"),
    ("Add shot", "添加镜头"),
    ("Edit shot", "编辑镜头"),
    ("Open scene", "打开场景"),
    ("Several changes", "多项修改"),
    // templates
    ("Fresh aisle", "生鲜通道"),
    (
        "A fresh-food aisle: produce stand, melons, chillers, a shelf, a shopper reaching and a crouching clerk; a slow push-in",
        "生鲜通道：果蔬台、西瓜、冷柜、货架、伸手拿货的顾客和蹲着的店员；缓慢推近",
    ),
    ("Product shot", "产品拍摄台"),
    (
        "Infinity cove, stepped podium, hero bottle and jar with leaves and pebbles, softbox lighting; a low close-up with a 360 spin",
        "无缝影棚、阶梯展台、主角瓶子和罐子配叶子和石子，柔光箱布光；低角度特写加 360 度环绕",
    ),
    ("White packshot", "白底产品图"),
    (
        "White cove, a drinks can on a plinth with ice, a tube on an acrylic riser, softbox and reflector; a hero arc then a detail push-in",
        "白色无缝影棚，展台上的饮料罐配冰块，亚克力台上的软管，柔光箱和反光板；主镜头环绕再接细节推近",
    ),
    ("Flat lay", "俯拍平铺"),
    (
        "A breakfast table seen from straight above: tray, coffee, jar, citrus, flowers and a carton under a soft side light",
        "从正上方拍的早餐桌：托盘、咖啡、罐子、柑橘、花和纸盒，柔和的侧光",
    ),
    ("Interview", "双人访谈"),
    (
        "Two people sitting face to face with a lamp and a wall behind; three-point light; two over-the-shoulder cameras and a wide",
        "两人面对面坐着，背后有台灯和墙；三点布光；两个过肩镜头和一个全景",
    ),
    ("Street corner", "街角"),
    (
        "A car, trees and a doorway; a person walking past; a tracking shot and a drone flyover; golden-hour sun",
        "汽车、树和门洞；一个人走过；跟拍镜头和无人机飞越；黄金时刻的阳光",
    ),
    ("Cafe", "咖啡馆"),
    (
        "Tables and chairs, two people talking, a counter with a clerk; a gentle arc around them",
        "桌椅、两人交谈、柜台和店员；缓缓环绕他们",
    ),
    ("Empty studio", "空摄影棚"),
    (
        "Just an infinity cove, a camera and three-point light, ready for your own blocks",
        "只有无缝影棚、一台相机和三点布光，等你放素材",
    ),
    // poses
    ("Stand", "站立"),
    ("Relaxed standing, arms down", "放松站立，手臂下垂"),
    ("T pose", "T 字姿势"),
    ("Arms straight out (for checking the rig)", "双臂平伸（检查骨骼用）"),
    ("Walk", "走路"),
    ("Mid-stride, left leg forward", "迈步中，左腿在前"),
    ("Run", "跑步"),
    ("Running stride", "跑步的步伐"),
    ("Sit", "坐"),
    ("Sitting on a chair (seat ≈ 0.45 m), hands on the thighs", "坐在椅子上（座高约 0.45 米），手放在大腿上"),
    ("Crouch", "蹲下"),
    ("Crouching down (to a low shelf or crate)", "蹲下（够低处的货架或箱子）"),
    ("Wave", "挥手"),
    ("Waving with the right hand", "用右手挥手"),
    ("Pointing ahead with the right arm", "用右臂指向前方"),
    ("Phone", "手机"),
    ("Reach", "够高处"),
    ("Reaching up to a high shelf with the right hand", "右手伸向高处的货架"),
    ("Pick up", "捡起"),
    ("Bending forward to pick from a crate or freezer", "弯腰从箱子或冰柜里拿东西"),
    ("Hold basket", "挎篮子"),
    ("Carrying a basket on the bent left arm", "左臂弯着挎一个篮子"),
    ("Push cart", "推车"),
    ("Both arms forward on a trolley handle", "双手向前扶着购物车把手"),
    ("Phone at the right ear", "手机放在右耳边"),
    ("Arms crossed", "抱臂"),
    ("Arms folded across the chest", "双臂交叉抱在胸前"),
    ("Present", "展示"),
    ("Presenting a product with both hands, palms up", "双手掌心向上展示产品"),
    // motion presets
    ("Dolly in", "推轨前进"),
    ("Move the camera forward along its view", "沿视线方向把相机向前推"),
    ("Dolly out", "推轨后退"),
    ("Move the camera back along its view", "沿视线方向把相机向后拉"),
    ("Push in", "推近"),
    ("Slow push towards the subject, keeping it centred", "缓慢推向主体，保持居中"),
    ("Truck left", "左移"),
    ("Slide the camera sideways to the left", "相机向左平移"),
    ("Truck right", "右移"),
    ("Slide the camera sideways to the right", "相机向右平移"),
    ("Pedestal up", "升机位"),
    ("Raise the camera straight up", "相机垂直升起"),
    ("Pedestal down", "降机位"),
    ("Lower the camera straight down", "相机垂直降下"),
    ("Pan left", "左摇"),
    ("Turn the camera to the left on the spot", "相机原地向左转"),
    ("Pan right", "右摇"),
    ("Turn the camera to the right on the spot", "相机原地向右转"),
    ("Tilt up", "上仰"),
    ("Tilt the camera up on the spot", "相机原地向上仰"),
    ("Tilt down", "下俯"),
    ("Tilt the camera down on the spot", "相机原地向下俯"),
    ("Arc left", "左环绕"),
    ("Circle around the subject to the left, looking at it", "看着主体向左环绕"),
    ("Arc right", "右环绕"),
    ("Circle around the subject to the right, looking at it", "看着主体向右环绕"),
    ("Crane up", "摇臂上升"),
    ("Rise up while keeping the subject in view", "上升同时保持主体在画面中"),
    ("Crane down", "摇臂下降"),
    ("Come down while keeping the subject in view", "下降同时保持主体在画面中"),
    ("Zoom in", "变焦推近"),
    ("Zoom the lens in (longer focal length)", "镜头变焦推近（焦距变长）"),
    ("Zoom out", "变焦拉远"),
    ("Zoom the lens out (shorter focal length)", "镜头变焦拉远（焦距变短）"),
    ("Whip pan", "甩镜"),
    ("A very fast pan", "非常快的横摇"),
    ("Tracking", "跟拍"),
    ("Follow the (animated) subject, keeping the same offset", "跟随（运动中的）主体，保持相同距离"),
    ("Handheld", "手持"),
    ("Add a gentle handheld shake on top of the camera's motion", "在相机运动上叠加轻微的手持晃动"),
    ("360 spin", "360 度环绕"),
    ("Circle once around the subject at a constant speed", "匀速绕主体一圈"),
    ("Spiral up", "螺旋上升"),
    ("Circle half-way around the subject while rising, looking at it", "看着主体边上升边绕半圈"),
    ("Chase", "追拍"),
    ("Follow behind the (moving) subject, looking where it goes", "跟在（移动的）主体后面，看着它前进的方向"),
    ("Top-down", "升到正上方"),
    ("Rise until straight above the subject, looking down", "上升到主体正上方，向下看"),
    ("Turntable", "转台"),
    ("Spin the object once around its vertical axis", "物体绕竖直轴转一圈"),
    ("Pulse", "脉动"),
    ("Beat in size three times, like a heartbeat", "像心跳一样缩放三次"),
    ("Shake", "抖动"),
    ("A quick nervous shake that dies down", "一阵快速抖动然后停下"),
    ("Swing", "摆动"),
    ("Swing like a hanging sign, settling", "像挂着的招牌一样摆动并停稳"),
    ("Flip", "翻转"),
    ("Hop and do a full forward flip", "跳起来向前翻一圈"),
    ("Bounce", "弹跳"),
    ("Hop up and down a few times, settling", "上下跳几下然后停稳"),
    ("Float", "悬浮"),
    ("Hover gently up and down with a slight sway", "轻轻上下浮动并略微摇摆"),
    ("Drop in", "落入"),
    ("Fall into place from above with a small bounce", "从上方落到位置并轻轻弹一下"),
    ("Slide in", "滑入"),
    ("Slide into place from the side", "从侧面滑到位置"),
    ("Pop in", "弹出"),
    ("Grow from nothing with a springy overshoot", "从无到有弹性放大"),
    ("Wobble", "摇晃"),
    ("Rock from side to side, settling", "左右摇晃然后停稳"),
    ("Hero reveal", "主角登场"),
    ("Rise up, turn to the front and grow into place", "升起、转到正面并放大到位"),
    ("Follow path", "沿路径移动"),
    ("Move through `points`, facing the way it goes", "沿 `points` 移动，朝向前进方向"),
    ("Walk forward with a stepping cycle", "迈步向前走"),
    ("Run forward with long strides", "大步向前跑"),
    ("Raise the right hand and wave", "举起右手挥动"),
    ("Raise the right arm, point ahead, lower it again", "抬起右臂指向前方再放下"),
    ("Talk", "说话"),
    ("Small hand gestures and nods, as if talking", "小幅手势加点头，像在说话"),
    ("Bend down to pick something up and stand again", "弯腰捡东西再站起来"),
    ("Look around", "左右张望"),
    ("Turn the head left and right", "头向左右转"),
    ("Nod", "点头"),
    ("Nod twice", "点头两次"),
    ("Sit down", "坐下"),
    ("Sit down from standing", "从站着坐下"),
    ("Stand up", "站起"),
    ("Stand up from sitting", "从坐着站起来"),
    // prefabs
    ("Man", "男人"),
    (
        "Rigged man, 1.76 m, short hair; pose with pose.set, front = +Z",
        "带骨骼的男人，1.76 米，短发；用 pose.set 摆姿势，正面朝 +Z",
    ),
    ("Woman", "女人"),
    (
        "Rigged woman, 1.65 m, long hair; pose with pose.set, front = +Z",
        "带骨骼的女人，1.65 米，长发；用 pose.set 摆姿势，正面朝 +Z",
    ),
    ("Person", "人"),
    ("Table", "桌子"),
    ("Dining table 1.2 × 0.8 m, 0.75 m high", "餐桌 1.2 × 0.8 米，高 0.75 米"),
    ("Chair", "椅子"),
    ("Dining chair, seat at 0.45 m", "餐椅，座高 0.45 米"),
    ("Sofa", "沙发"),
    ("Three-seat sofa, 2 m wide", "三人沙发，宽 2 米"),
    ("Bed", "床"),
    ("Double bed 1.6 × 2.1 m", "双人床 1.6 × 2.1 米"),
    ("Lamp", "落地灯"),
    ("Floor lamp, 1.75 m", "落地灯，1.75 米"),
    ("Box", "箱子"),
    ("Box 0.6 m", "箱子 0.6 米"),
    ("Stairs", "楼梯"),
    ("Straight stairs, 10 steps of 18 cm", "直楼梯，10 级，每级 18 厘米"),
    ("Wall", "墙"),
    ("Wall 4 m wide, 2.6 m high", "墙，宽 4 米，高 2.6 米"),
    ("Doorway", "门洞"),
    ("Wall 4 m wide with a 0.9 × 2.1 m opening", "宽 4 米的墙，带 0.9 × 2.1 米的门洞"),
    ("Car", "汽车"),
    ("Car 4.4 m long", "汽车，长 4.4 米"),
    ("Tree", "树"),
    ("Tree, about 3.9 m tall", "树，高约 3.9 米"),
    ("Desk", "办公桌"),
    ("Office desk 1.4 × 0.7 m, 0.74 m high", "办公桌 1.4 × 0.7 米，高 0.74 米"),
    ("Monitor", "显示器"),
    ("Desktop monitor 60 cm on a stand", "带支架的 60 厘米显示器"),
    ("Laptop", "笔记本电脑"),
    ("Open laptop 33 cm", "打开的笔记本电脑 33 厘米"),
    ("Desk chair", "办公椅"),
    ("Swivel desk chair", "转椅"),
    ("Bench", "长椅"),
    ("Park bench 1.6 m", "公园长椅 1.6 米"),
    ("Street lamp", "路灯"),
    ("Street lamp 4 m", "路灯 4 米"),
    ("Bicycle", "自行车"),
    ("Bicycle 1.7 m long, front at +Z", "自行车，长 1.7 米，车头朝 +Z"),
    ("Traffic cone 70 cm", "交通锥 70 厘米"),
    ("Fence", "栅栏"),
    ("Picket fence section 2 m", "尖桩栅栏一段 2 米"),
    ("Plant", "盆栽"),
    ("Potted plant 1 m", "盆栽植物 1 米"),
    ("Produce", "果蔬台"),
    (
        "Two-tier produce stand 1.6 × 1.0 m with crates of apples, oranges and greens",
        "两层果蔬台 1.6 × 1.0 米，放着苹果、橙子和青菜",
    ),
    ("Apples", "苹果"),
    ("Crate of red apples 0.6 × 0.4 m", "一箱红苹果 0.6 × 0.4 米"),
    ("Oranges", "橙子"),
    ("Crate of oranges 0.6 × 0.4 m", "一箱橙子 0.6 × 0.4 米"),
    ("Greens", "青菜"),
    ("Crate of leafy greens and cucumbers 0.6 × 0.4 m", "一箱绿叶菜和黄瓜 0.6 × 0.4 米"),
    ("Melons", "西瓜"),
    ("Cardboard bin of watermelons 1 × 1 m", "一纸箱西瓜 1 × 1 米"),
    ("Shelf", "货架"),
    ("Gondola shelf 1.2 m wide, 1.8 m high, stocked with packs", "中岛货架，宽 1.2 米，高 1.8 米，摆满商品"),
    ("Chiller", "冷柜"),
    ("Open multideck chiller 1.25 m wide, 2.1 m high, with bottles", "开放式多层冷柜，宽 1.25 米，高 2.1 米，放着饮料"),
    ("Freezer", "冰柜"),
    ("Island chest freezer 2 × 0.9 m with a glass lid", "岛柜冰柜 2 × 0.9 米，玻璃盖"),
    ("Deli", "熟食柜"),
    ("Serve-over meat / deli counter 2 m wide with a sloped glass front", "肉类 / 熟食柜台，宽 2 米，斜面玻璃"),
    ("Fish", "海鲜台"),
    ("Fish-on-ice display 1.8 m wide, tilted to the customer", "冰鲜鱼展台，宽 1.8 米，朝顾客倾斜"),
    ("Bread", "面包架"),
    ("Bakery rack 1.2 m wide with sloped shelves of loaves", "面包架，宽 1.2 米，斜层放着面包"),
    ("Checkout", "收银台"),
    (
        "Checkout counter 1.8 m with belt, screen and card reader; cashier stands at -Z",
        "收银台 1.8 米，带传送带、屏幕和刷卡机；收银员站在 -Z 一侧",
    ),
    ("Cart", "购物车"),
    ("Shopping trolley, handle at -Z", "购物车，把手在 -Z 一侧"),
    ("Basket", "购物篮"),
    ("Hand basket 0.45 × 0.3 m", "手提购物篮 0.45 × 0.3 米"),
    ("Produce weighing scale with display", "带显示屏的果蔬电子秤"),
    ("Sign", "价签"),
    ("Price sign on a stand, 1.3 m", "立式价签，1.3 米"),
    ("Pallet", "托盘货"),
    ("Pallet 1.2 × 1.0 m stacked with cartons", "堆满纸箱的托盘 1.2 × 1.0 米"),
    (
        "Smartphone 7 × 15 cm (screen faces +Z); hold it with Hold in hand",
        "智能手机 7 × 15 厘米（屏幕朝 +Z）；用「拿在手里」握住",
    ),
    ("Bag", "购物袋"),
    ("Paper shopping bag with handles, 32 cm", "带提手的纸袋，32 厘米"),
    ("Handbag", "手提包"),
    ("Handbag with a strap, 28 cm", "带肩带的手提包，28 厘米"),
    ("Bottle", "瓶子"),
    (
        "Hero bottle stand-in 7 cm × 25 cm (use `size` to match the real product)",
        "主角瓶子替身 7 × 25 厘米（用 `size` 匹配真实产品）",
    ),
    ("Jar", "罐子"),
    ("Jar with lid, 8 cm × 10 cm", "带盖罐子，8 × 10 厘米"),
    ("Can", "易拉罐"),
    ("Drinks can 6.6 cm × 12 cm", "饮料罐 6.6 × 12 厘米"),
    ("Carton", "纸盒"),
    ("Product box / carton 12 × 18 × 5 cm", "产品盒 / 纸盒 12 × 18 × 5 厘米"),
    ("Tube", "软管"),
    ("Cosmetics tube standing on its cap, 16 cm", "倒立的化妆品软管，16 厘米"),
    ("Pouch", "自立袋"),
    ("Stand-up pouch 14 × 20 cm", "自立袋 14 × 20 厘米"),
    ("Cup", "咖啡杯"),
    ("Coffee cup with lid, 12 cm", "带盖咖啡杯，12 厘米"),
    ("Gift box", "礼盒"),
    ("Gift box with ribbon 20 cm", "带丝带的礼盒 20 厘米"),
    ("Pump", "按压瓶"),
    ("Cosmetics bottle with pump, 18 cm", "带泵头的化妆品瓶，18 厘米"),
    ("Podium", "展台"),
    ("Round plinth 40 cm across, 20 cm high", "圆形展台，直径 40 厘米，高 20 厘米"),
    ("Steps", "阶梯台"),
    ("Two-step round podium for a product line-up", "两级圆形展台，用于产品排列"),
    ("Riser", "方台"),
    ("Cube riser 20 cm", "方形垫台 20 厘米"),
    ("Pedestal", "立柱台"),
    ("Column pedestal 30 cm across, 90 cm high", "柱形展台，直径 30 厘米，高 90 厘米"),
    ("Cake stand", "蛋糕架"),
    ("Footed cake stand 28 cm", "高脚蛋糕架 28 厘米"),
    ("Tray", "托盘"),
    ("Serving tray 40 × 28 cm", "托盘 40 × 28 厘米"),
    ("Acrylic", "亚克力台"),
    ("Clear acrylic riser 30 × 15 cm", "透明亚克力垫台 30 × 15 厘米"),
    ("Arch", "拱形背景"),
    ("Arched backdrop panel 1.2 m wide, 2.2 m high", "拱形背景板，宽 1.2 米，高 2.2 米"),
    ("Cove", "无缝影棚"),
    ("Seamless infinity cove: 4 m wide floor curving up into a 3 m wall", "无缝影棚：4 米宽的地面弧形连到 3 米高的墙"),
    ("Paper roll", "背景纸"),
    ("Backdrop paper roll on stands, 2.7 m wide sweep", "架子上的背景纸卷，2.7 米宽"),
    ("Shoot table", "拍摄台"),
    ("Tabletop for product shots 1.0 × 0.7 m, 0.75 m high", "产品拍摄台 1.0 × 0.7 米，高 0.75 米"),
    ("Softbox", "柔光箱"),
    (
        "Softbox on a light stand, head at 1.8 m, aimed forward (+Z); it is a real area light",
        "灯架上的柔光箱，灯头高 1.8 米，朝前（+Z）；是真正的面光源",
    ),
    ("Reflector", "反光板"),
    ("Round reflector card 80 cm on a stand", "支架上的圆形反光板 80 厘米"),
    ("Leaves", "树枝"),
    ("Small branch with leaves, 35 cm", "带叶子的小树枝，35 厘米"),
    ("Flower", "花"),
    ("Single flower on a stem, 30 cm", "一枝花，30 厘米"),
    ("Slice", "橙子片"),
    ("Orange slice 7 cm", "橙子片 7 厘米"),
    ("Pebbles", "石子"),
    ("A few smooth stones", "几块光滑的石头"),
    ("Ice", "冰块"),
    ("Scattered ice cubes", "散落的冰块"),
    ("Drape", "布料"),
    ("Folded fabric drape 1 × 0.6 m", "折叠的布料 1 × 0.6 米"),
    ("Ribbon", "丝带"),
    ("Ribbon loop 30 cm", "丝带环 30 厘米"),
    ("Splash", "水花"),
    ("Water splash stand-in (crown and droplets), 25 cm", "水花替身（水冠和水滴），25 厘米"),
    ("Spheres", "装饰球"),
    ("Three decor spheres of different sizes", "三个不同大小的装饰球"),
    ("Traffic cone", "交通锥"),
    // mesh edit mode
    ("Edit mesh", "编辑网格"),
    ("✏ Edit mesh", "✏ 编辑网格"),
    ("Edit vertices, edges and faces: extrude, bevel, loop cut… (Tab)", "编辑顶点、边和面：挤出、倒角、环切…（Tab）"),
    ("Edit mode", "编辑模式"),
    ("Vertex", "顶点"),
    ("Edge", "边"),
    ("Pick vertices (1)", "选顶点（1）"),
    ("Pick edges (2)", "选边（2）"),
    ("Pick faces (3)", "选面（3）"),
    ("X-ray", "透视"),
    ("See and pick through the mesh (Alt+Z)", "透过网格查看和选择（Alt+Z）"),
    ("◎ Proportional", "◎ 衰减编辑"),
    (
        "Moving, rotating or scaling also pulls nearby vertices, fading out (O; scroll while moving to resize)",
        "移动、旋转或缩放时也带动附近的顶点，逐渐减弱（O；移动时滚轮调范围）",
    ),
    ("Done", "完成"),
    ("Back to objects (Tab)", "回到物体（Tab）"),
    ("All", "全选"),
    ("Linked", "相连"),
    ("Everything joined to the selection (Ctrl+L; L under the pointer)", "与选中部分相连的全部（Ctrl+L；指针下按 L）"),
    ("More", "扩大"),
    ("Grow by one step (Ctrl +)", "向外扩一圈（Ctrl +）"),
    ("Loop", "循环边"),
    ("Edge loop through the first selected edge (Alt+click)", "经过第一条选中边的循环边（Alt+单击）"),
    ("Ring", "并排边"),
    ("Edge ring across the first selected edge", "横穿第一条选中边的并排边"),
    ("Extrude", "挤出"),
    ("E: drag it out · or apply the distance here", "E：拖出来 · 或在这里按距离挤出"),
    ("Extrude and follow the pointer (E)", "挤出并跟随指针（E）"),
    ("Inset", "内插"),
    ("I: a smaller face inside", "I：在面内插入一个小面"),
    ("Bevel", "倒角"),
    ("Ctrl+B: round off edges (faces: their edges)", "Ctrl+B：把边倒圆（选面时倒它们的边）"),
    ("Loop cut", "环切"),
    (
        "Ctrl+R or C: hover an edge, scroll for more cuts, click to cut",
        "Ctrl+R 或 C：指向一条边，滚轮增加刀数，单击切割",
    ),
    ("Edge slide", "滑边"),
    ("G G: slide the selected edges along their neighbours", "G G：让选中的边沿相邻边滑动"),
    ("Knife", "切刀"),
    (
        "K: click two points to cut along the line (the faces it crosses, or the selected faces)",
        "K：点两个点沿线切开（切它经过的面，或选中的面）",
    ),
    ("Bridge", "桥接"),
    (
        "Join two border loops with faces; with two faces selected, makes a tunnel between them",
        "用面连接两圈边界边；选中两个面时在它们之间打通",
    ),
    (
        "Extrude the selected edges or faces round the object's origin (360° joins up)",
        "绕物体原点旋转挤出选中的边或面（360° 会闭合）",
    ),
    ("O: G / R / S also pull nearby vertices", "O：G / R / S 也带动附近的顶点"),
    ("Subdivide", "细分"),
    ("Split the selected faces into smaller quads", "把选中的面分成更小的四边形"),
    ("Connect", "连接"),
    ("J: a new edge between two selected vertices of a face", "J：在面上两个选中顶点之间连一条新边"),
    ("Merge", "合并"),
    ("M: merge the selected vertices at their centre", "M：把选中的顶点合并到中心"),
    ("F: make a face from a ring of border edges or 3+ vertices", "F：用一圈边界边或 3 个以上顶点做一个面"),
    ("Turn the selected faces inside out", "把选中的面翻过来"),
    ("X / Delete: remove the selection", "X / Delete：删除选中部分"),
    ("Clean up", "清理"),
    (
        "Merge vertices closer than 0.1 mm (whole mesh): fixes split imported meshes",
        "合并距离小于 0.1 毫米的顶点（整个网格）：修复导入后裂开的网格",
    ),
    (
        "Click to pick · Shift+click add · Alt+click loop · Cmd/Ctrl+drag box · drag a selection to move it · G R S move, rotate, scale (X Y Z lock, type a number) · G G edge slide · K knife · double-click or L linked",
        "单击选择 · Shift+单击加选 · Alt+单击选循环边 · Cmd/Ctrl+拖动框选 · 拖动选中部分可移动 · G R S 移动、旋转、缩放（X Y Z 锁定轴，可输入数值）· G G 滑边 · K 切刀 · 双击或 L 选相连",
    ),
    ("Rotate", "旋转"),
    (
        "Select a mesh, then press Tab to edit its vertices, edges and faces",
        "先选中一个网格，再按 Tab 编辑它的顶点、边和面",
    ),
    (
        "This is a group: click again to pick one part, then press Tab",
        "这是一个组：再点一次选中其中一个部件，然后按 Tab",
    ),
    (
        "Edit mode · 1 vertex / 2 edge / 3 face · G move · E extrude · Tab to finish",
        "编辑模式 · 1 顶点 / 2 边 / 3 面 · G 移动 · E 挤出 · Tab 结束",
    ),
    ("Edge slide: no neighbouring edges to slide along", "滑边：没有可以滑动的相邻边"),
    ("Proportional editing on (scroll while moving to resize)", "衰减编辑已开（移动时滚轮调范围）"),
    ("Proportional editing off", "衰减编辑已关"),
    ("Knife: click at least two points", "切刀：至少点两个点"),
    ("hold over a vertex to snap to it", "停在顶点上可吸附"),
    ("click where the cut starts", "点击切割的起点"),
    ("click the next point (inside a face to turn a corner)", "点击下一个点（点在面内可拐弯）"),
    ("release to apply", "松开即应用"),
    ("click or Enter to apply", "单击或按 Enter 应用"),
    (
        " · X Y Z lock an axis · type a number · scroll: proportional radius",
        " · X Y Z 锁定轴 · 输入数值 · 滚轮：衰减范围",
    ),
    (
        " · X Y Z lock an axis · type a number · hold Cmd/Ctrl to snap to a vertex",
        " · X Y Z 锁定轴 · 输入数值 · 按住 Cmd/Ctrl 吸附到顶点",
    ),
    (" · X Y Z lock an axis · type a number", " · X Y Z 锁定轴 · 输入数值"),
    (" · type a number", " · 输入数值"),
    ("depth ", "深度 "),
    (" seg", " 段"),
    (" cuts", " 刀"),
    // boolean
    ("Boolean", "布尔"),
    ("Add a second mesh to cut with or merge in.", "再加一个网格，用来挖切或合并。"),
    ("With", "和"),
    ("Difference", "差集"),
    ("Carve the other mesh out of this one", "从这个网格里挖掉另一个"),
    ("Union", "并集"),
    ("Merge both into one solid", "合并成一个实体"),
    ("Intersect", "交集"),
    ("Keep only where they overlap", "只保留重叠部分"),
    (
        "Changes this mesh and hides the other one. Both should be closed solids.",
        "会修改这个网格并隐藏另一个。两者都应是封闭实体。",
    ),
    // mesh, modifiers
    ("Array", "阵列"),
    ("+ Mirror X", "+ 镜像 X"),
    ("+ Array", "+ 阵列"),
    ("+ Subdivide", "+ 细分"),
    // texture and UVs
    ("Texture", "贴图"),
    ("Picture", "图片"),
    ("none", "无"),
    ("No pictures in the scene yet", "场景里还没有图片"),
    ("Load…", "载入…"),
    ("Use a PNG, JPEG or WebP file as this mesh's texture", "用 PNG、JPEG 或 WebP 文件做这个网格的贴图"),
    ("Remove the texture", "移除贴图"),
    ("Give the mesh a picture: a label, wood grain, a packaging print…", "给网格贴一张图：标签、木纹、包装印刷…"),
    ("Laid on as", "贴法"),
    ("Box: each face takes the picture from the side it faces most", "盒状：每个面取它最朝向那一侧的投影"),
    ("Planar: projected straight along one axis", "平面：沿一个轴直接投影"),
    ("Cylinder: wrapped around an axis, like a can label", "圆柱：绕轴包裹，像罐头标签"),
    ("Sphere: wrapped like a globe", "球面：像地球仪一样包裹"),
    (
        "UV map: follows the mesh's own UVs (from Unwrap or an imported model)",
        "UV：使用网格自带的 UV（来自展开或导入的模型）",
    ),
    ("Axis", "轴"),
    ("Repeat", "重复"),
    ("Offset", "偏移"),
    ("selected faces", "选中的面"),
    ("whole mesh", "整个网格"),
    ("Cut into flat pieces and pack them without overlap", "切成平展的小块并不重叠地排好"),
    ("Bake a box projection into the UV map", "把盒状投影烘焙进 UV"),
    ("Bake a cylinder projection (can, bottle)", "烘焙圆柱投影（罐子、瓶子）"),
    ("Bake a sphere projection", "烘焙球面投影"),
    ("Turn the picture on these faces", "旋转这些面上的图片"),
    ("Tile ×2", "平铺 ×2"),
    ("Show twice as much of the picture", "图片显示两倍的范围"),
    ("Zoom ×2", "放大 ×2"),
    ("Zoom into the picture", "放大图片"),
    ("Mirror", "镜像"),
    ("Mirror the picture left-right", "左右镜像图片"),
    (
        "Tip: in edit mode, pick faces to unwrap or nudge just those.",
        "提示：在编辑模式下选中一些面，可以只展开或调整这些面。",
    ),
    // realistic people
    ("Hands", "手"),
    ("Body", "身体"),
    ("Clothes", "服装"),
    ("Female ↔ male", "女 ↔ 男"),
    ("0 = female, 1 = male", "0 = 女，1 = 男"),
    ("Age", "年龄"),
    ("Years", "岁"),
    ("Weight", "胖瘦"),
    ("Thin … heavy", "瘦 … 胖"),
    ("Muscle", "肌肉"),
    ("Slight … muscular", "纤细 … 健壮"),
    ("Short … tall", "矮 … 高"),
    ("Bottom", "下装"),
    ("Shoes", "鞋"),
    ("Apron", "围裙"),
    ("Skin", "皮肤"),
    ("Eyes", "眼睛"),
    ("sleeves", "袖长"),
    ("legs", "裤长"),
    ("Keyed: the face cross-fades between expression keys.", "已设关键帧：面部会在表情关键帧之间渐变。"),
    ("More poses ▾", "更多姿势 ▾"),
    ("Realistic man", "写实男性"),
    ("Realistic woman", "写实女性"),
    (
        "Realistic rigged man (MakeHuman), 1.75 m, with fingers and facial expressions; shape with person.setBody / setOutfit / setExpression",
        "带骨骼的写实男性（MakeHuman），1.75 米，含手指和面部表情；可用 person.setBody / setOutfit / setExpression 调整",
    ),
    (
        "Realistic rigged woman (MakeHuman), 1.62 m, shoulder-length hair, fingers and facial expressions",
        "带骨骼的写实女性（MakeHuman），1.62 米，齐肩发，含手指和面部表情",
    ),
    ("Realistic man in a light work shirt and green apron: shop assistant", "穿浅色工作衬衫、绿色围裙的写实男性：店员"),
    ("Realistic man in his seventies, grey hair, long sleeves", "七十多岁的写实男性，灰发，长袖"),
    (
        "Realistic man in whites and a white apron: deli counter, butcher, kitchen",
        "穿白色工作服和白围裙的写实男性：熟食柜台、肉铺、厨房",
    ),
    ("Realistic muscular man in sportswear (tank top, shorts)", "穿运动装（背心、短裤）的健壮写实男性"),
    ("Realistic woman around seventy, grey hair, long sleeves", "七十岁左右的写实女性，灰发，长袖"),
    (
        "Realistic woman in her late thirties, casual long sleeves: a supermarket customer",
        "三十多岁的写实女性，休闲长袖：超市顾客",
    ),
    ("Realistic young man in a yellow tee, chinos and white trainers", "穿黄色 T 恤、卡其裤和白球鞋的写实年轻男性"),
    (
        "Realistic young woman in a red uniform polo: cashier, promoter",
        "穿红色制服 Polo 衫的写实年轻女性：收银员、促销员",
    ),
    ("Realistic boy, about 11 years, 1.4 m", "写实男孩，约 11 岁，1.4 米"),
    ("Realistic girl, about 11 years, 1.4 m", "写实女孩，约 11 岁，1.4 米"),
    ("Alias of `man`", "`man` 的别名"),
    // other
    ("GPU not available", "GPU 不可用"),
    ("drag to aim", "拖动调整朝向"),
    (" with a skeleton (click it, then a part, to pose)", "，带骨骼（点它，再点某个部位即可摆姿势）"),
    ("Make active", "设为当前相机"),
    ("Look at origin", "看向原点"),
    ("Delete / Backspace", "Delete / 退格键"),
    ("GLB (glTF binary)", "GLB（glTF 二进制）"),
    ("glTF (text)", "glTF（文本）"),
    ("FBX (via Blender)", "FBX（经 Blender）"),
    ("3-point", "三点布光"),
    ("Key, fill and rim around the selection (or the whole scene)", "围绕选中物体（或整个场景）布置主光、补光和轮廓光"),
    ("Two side softboxes and one overhead: even product light", "两侧柔光箱加顶光：均匀的产品光"),
    ("Vase", "花瓶"),
    ("Bowl", "碗"),
    // mesh edit mode
    ("Face", "面"),
    ("Proportional", "衰减编辑"),
    ("None", "无"),
    ("Spin", "旋转挤出"),
    ("Fill", "填充"),
    (" steps", " 步"),
    // boolean
    ("Apply", "应用"),
    // mesh, modifiers
    (" smooth", " 平滑"),
    // texture and UVs
    ("Smart", "智能"),
    // realistic people
    ("Top", "上衣"),
    // other
    ("Interface language: English", "界面语言：英文"),
    ("Product", "产品"),
    // poses
    ("Bow", "鞠躬"),
    ("Bowing politely", "礼貌地鞠躬"),
    ("Cheer", "欢呼"),
    ("Both arms up, celebrating", "双臂举起庆祝"),
    ("Scan", "扫码"),
    ("At a checkout, scanning an item with the right hand", "在收银台用右手扫商品"),
    ("Bag items", "装袋"),
    ("Packing items into a bag on the counter", "在柜台上把商品装进袋子"),
    ("Taste", "试吃"),
    ("Bringing food or a sample to the mouth", "把食物或试吃品送到嘴边"),
    ("Carry bag", "拎袋子"),
    ("Carrying a shopping bag in the right hand", "右手拎着购物袋"),
    ("Carry box", "搬箱子"),
    ("Carrying a box or crate in front with both arms", "双臂在身前抱着箱子或筐"),
    ("Check phone", "看手机"),
    ("Looking down at a phone held in both hands", "低头看双手拿着的手机"),
    ("Clap", "鼓掌"),
    ("Clapping hands in front of the chest", "在胸前拍手"),
    ("Drink", "喝水"),
    ("Drinking from a cup or bottle", "用杯子或瓶子喝水"),
    ("Explain", "讲解"),
    ("Talking with open hands, explaining", "摊开双手说话、讲解"),
    ("Facepalm", "捂脸"),
    ("Hand over the face (oops, frustration)", "手捂着脸（糟了、懊恼）"),
    ("Hand over", "递东西"),
    ("Handing something over with both hands at chest height", "双手在胸前高度递出东西"),
    ("Hands on hips", "叉腰"),
    ("Hands on the hips, elbows out (confident)", "双手叉腰，手肘向外（自信）"),
    ("Inspect", "端详"),
    ("Holding produce up in both hands to look at it", "双手举起果蔬仔细看"),
    ("Kneel", "单膝跪"),
    ("Kneeling on the right knee (to a low shelf, to a child)", "右膝跪地（够低处货架、和孩子说话）"),
    (
        "Laugh: head back, shoulders shaking (realistic people laugh with their face too)",
        "大笑：头后仰、肩膀抖动（写实人物脸上也会笑）",
    ),
    ("Lean counter", "趴柜台"),
    ("Leaning forward on a counter on the forearms", "前臂撑在柜台上向前倾"),
    ("Look up shelf", "仰望货架"),
    ("Standing back, looking up at a high shelf", "退后一步，抬头看高处的货架"),
    ("Mid-stride, right leg forward (the other half of the walk cycle)", "迈步中，右腿在前（走路循环的另一半）"),
    ("Pay", "付款"),
    ("Paying: holding a phone or card out to the reader", "付款：把手机或卡伸向读卡器"),
    ("Receive", "接东西"),
    ("Reaching out to take something, palms up", "伸手去接东西，掌心向上"),
    ("Recommend", "推荐"),
    ("Showing a product to the side with an open hand", "张开手向一侧展示商品"),
    ("Restock", "上货"),
    ("Placing a product on a shelf at shoulder height", "把商品放到齐肩高的货架上"),
    ("Selfie", "自拍"),
    ("Taking a selfie with the right hand", "右手举着自拍"),
    ("Shrug", "耸肩"),
    ("Shrugging, palms up", "耸肩，掌心向上"),
    ("Sit floor", "坐地上"),
    ("Sitting on the floor, legs out, leaning on the hands", "坐在地上，腿伸直，手撑在身后"),
    ("Stretch", "伸懒腰"),
    ("Stretching both arms overhead", "双臂举过头顶伸展"),
    ("Thinking", "思考"),
    ("Hand on the chin, the other arm across the waist", "一手托下巴，另一臂横在腰前"),
    ("Wait in line", "排队"),
    ("Waiting with hands clasped in front", "双手交握在身前等待"),
    ("Welcome", "迎客"),
    ("Greeting a customer: open hand out, a slight bow", "招呼顾客：伸出手，微微欠身"),
    ("Weight on one leg, hip shifted (contrapposto)", "重心在一条腿上，胯部偏移（对立式站姿）"),
    (
        "Working at a counter: both hands forward, looking down (chopping, packing)",
        "在台面前干活：双手向前，低头（切菜、打包）",
    ),
    ("Chop", "切菜"),
    ("Cashier scanning an item over the counter", "收银员在柜台上扫商品"),
    ("Browse", "逛"),
    // hand shapes
    ("Relaxed", "放松"),
    ("Natural, slightly curled", "自然，微微弯曲"),
    ("Fist", "握拳"),
    ("Closed fist", "攥紧的拳头"),
    ("Open", "张开"),
    ("Flat open hand, fingers together", "手掌平展，手指并拢"),
    ("Spread", "张开五指"),
    ("Fingers spread wide (showing, stop)", "五指张开（展示、停）"),
    ("Index finger pointing", "食指指向"),
    ("Thumbs up", "竖大拇指"),
    ("Ok", "OK"),
    ("OK sign: thumb and index make a ring", "OK 手势：拇指和食指成圈"),
    ("Peace", "剪刀手"),
    ("V sign: index and middle up", "V 手势：竖起食指和中指"),
    ("Grip", "握持"),
    ("Holding a cup, bottle, phone or handle", "握着杯子、瓶子、手机或把手"),
    ("Pinch", "捏"),
    ("Picking something small between thumb and index", "用拇指和食指捏起小东西"),
    ("Claw", "爪"),
    ("Fingers bent like a claw (grabbing)", "手指弯成爪状（抓取）"),
    // expressions
    ("Relaxed, no expression", "放松，没有表情"),
    ("Smile", "微笑"),
    ("Friendly closed-mouth smile", "友好的闭嘴微笑"),
    ("Grin", "咧嘴笑"),
    ("Big smile showing the teeth", "露出牙齿的大笑脸"),
    ("Laugh", "大笑"),
    ("Laughing, mouth open, eyes narrowed", "张嘴大笑，眯着眼"),
    ("Delighted", "惊喜"),
    ("Pleasantly surprised (tasting, unboxing)", "惊喜（试吃、开箱）"),
    ("Surprised", "吃惊"),
    ("Brows up, eyes wide, mouth open", "挑眉、睁大眼、张嘴"),
    ("Sad", "难过"),
    ("Inner brows raised, mouth corners down", "眉头上扬，嘴角向下"),
    ("Angry", "生气"),
    ("Brows down, lips pressed", "皱眉，抿嘴"),
    ("Disgusted", "嫌弃"),
    ("Nose wrinkled, upper lip raised", "皱鼻子，上唇上提"),
    ("Fear", "害怕"),
    ("Brows up and together, eyes wide, mouth stretched", "眉头上扬并拢，睁大眼，嘴角拉开"),
    ("Focused", "专注"),
    ("Concentrating: brows slightly down, eyes narrowed", "专注：眉头微皱，眯眼"),
    ("Skeptical", "怀疑"),
    ("One eyebrow raised, unconvinced", "挑起一边眉毛，不太相信"),
    ("Thinking", "思考"),
    ("Pondering: one brow up, lips pursed to the side", "沉思：一边眉毛上挑，嘴撇向一边"),
    ("Talking", "说话"),
    ("Mid-sentence, mouth half open", "说话中，嘴半张"),
    ("Shout", "喊"),
    ("Shouting or cheering, mouth wide open", "大喊或欢呼，嘴张大"),
    ("Wink", "眨眼"),
    ("Winking with the right eye, half smile", "右眼眨眼，半笑"),
    ("Kiss", "嘟嘴"),
    ("Puckered lips (also blowing, whistling)", "嘟起嘴（也可是吹气、吹口哨）"),
    ("Eyes closed", "闭眼"),
    ("Eyes gently closed (sleeping, savouring)", "轻轻闭眼（睡觉、回味）"),
    // hair
    ("Short", "短发"),
    ("Long", "长发"),
    ("Bob", "波波头"),
    ("Ponytail", "马尾"),
    ("Bun", "发髻"),
    // people presets
    ("Older man", "年长男性"),
    ("Older woman", "年长女性"),
    ("Boy", "男孩"),
    ("Girl", "女孩"),
    ("Athlete", "运动员"),
    ("Cashier", "收银员"),
    ("Casual", "休闲装"),
    ("Shopper", "顾客"),
    ("Store staff", "店员"),
    ("Deli / chef", "熟食 / 厨师"),
    // motion presets
    ("Dolly zoom", "滑动变焦"),
    ("Vertigo: pull back while zooming in, the subject stays the same size", "眩晕效果：边后退边推焦，主体大小不变"),
    ("Drone flyover", "无人机飞越"),
    ("Fly in low from behind, rise over the subject and look down", "从后方低空飞入，升到主体上方并俯视"),
    ("Pull-back reveal", "拉远揭示"),
    (
        "Start close on the subject and pull back and up to reveal the scene",
        "从主体近景开始，向后向上拉开，揭示整个场景",
    ),
    ("Walk away", "走开"),
    // templates
    (
        "A checkout counter: the cashier scans and chats while a shopper pays by phone; a medium two-shot then a close-up on the payment",
        "收银台：收银员边扫码边聊天，顾客用手机付款；先中景双人镜头，再给付款特写",
    ),
    ("Reaching for a product on a shelf at chest height", "伸手去拿齐胸高货架上的商品"),
    // mesh edit mode
    // boolean
    // mesh, modifiers
    // texture and UVs
    ("box", "盒状"),
    ("planar", "平面"),
    ("cylinder", "圆柱"),
    ("sphere", "球面"),
    ("uv", "UV"),
    // realistic people
    // other
    // poses
    // hand shapes
    // expressions
    // hair
    // people presets
    // motion presets
    // templates
    ("Smooth", "平滑"),
    ("Sharp", "尖锐"),
    ("Constant", "恒定"),
    // mesh edit mode
    // boolean
    // mesh, modifiers
    // texture and UVs
    // realistic people
    // other
    // poses
    // hand shapes
    // expressions
    // hair
    // people presets
    // motion presets
    // templates
    ("drag", "拖动"),
    // mesh edit mode
    // boolean
    // mesh, modifiers
    // texture and UVs
    // realistic people
    // other
    // poses
    // hand shapes
    // expressions
    // hair
    // people presets
    // motion presets
    // templates
    // sky, shadows and lighting presets
    ("Sky", "天空"),
    ("Sky ▾", "天空 ▾"),
    ("Sky & shadows", "天空与阴影"),
    ("Sky light", "天空光"),
    ("Sky (built-in skies or an HDRI), sky light and shadows", "天空（内置天空或 HDRI）、天空光和阴影"),
    ("Studio", "摄影棚"),
    ("Color", "纯色"),
    ("Day", "白天"),
    ("Sunset", "日落"),
    ("Overcast", "阴天"),
    ("Night", "夜晚"),
    ("Hdri", "HDRI"),
    ("HDRI", "HDRI"),
    ("Neutral grey studio, no sky light (default)", "中性灰摄影棚，没有天空光（默认）"),
    ("One colour all round: seamless paper backdrop", "四周同一种颜色：无缝背景纸"),
    ("Clear blue sky, high sun", "晴朗蓝天，太阳很高"),
    ("Low warm sun, orange horizon", "低角度暖阳，橙色地平线"),
    ("Bright cloudy sky, soft even light", "明亮的多云天空，光线柔和均匀"),
    ("Dark blue night", "深蓝色夜晚"),
    ("Dark studio with softbox panels", "暗摄影棚加柔光板"),
    ("An imported HDRI / panorama (environment.import)", "导入的 HDRI / 全景图（environment.import）"),
    ("Import HDRI…", "导入 HDRI…"),
    ("Import HDRI", "导入 HDRI"),
    (
        "An .hdr file or a 2:1 panorama picture (you can also drop an .hdr onto the view)",
        "一个 .hdr 文件或 2:1 全景图（也可以把 .hdr 拖进视图）",
    ),
    ("Add matching sun", "添加匹配的太阳光"),
    (
        "A sun light from the sky's brightest spot, so shadows match (light.fromEnvironment)",
        "从天空最亮处添加一盏太阳光，让阴影对得上（light.fromEnvironment）",
    ),
    ("This sky has no clear sun", "这个天空没有明显的太阳"),
    ("Sun from sky", "从天空取太阳光"),
    ("Tint", "色调"),
    ("White", "白"),
    ("Grey", "灰"),
    ("Black", "黑"),
    ("Green", "绿"),
    ("Background", "背景"),
    ("Show the sky behind the scene", "在场景后面显示天空"),
    ("How brightly the sky lights the scene (the background keeps its look)", "天空照亮场景的强度（背景外观不变）"),
    ("Turn the sky and its sun about the vertical axis", "绕竖直轴旋转天空和它的太阳"),
    (
        "The sky lights everything softly from all round and adds to your lights; with no lights it lights the scene alone. Lighting presets set a matching sky.",
        "天空从四周柔和地照亮一切，并叠加在你的灯光上；没有灯光时由它单独照亮场景。布光预设会设置匹配的天空。",
    ),
    ("On", "开"),
    ("Darkness", "暗度"),
    ("Casts shadows", "投射阴影"),
    ("Up to four lights cast shadows at once (the brightest)", "最多四盏灯同时投射阴影（最亮的几盏）"),
    (
        "Show shadows in the viewport (snapshots follow the scene's shadow settings)",
        "在视图里显示阴影（截图按场景的阴影设置）",
    ),
    (
        "A light shines along its -Z. Scenes with lights are lit by them; toggle Studio light in the view toolbar to compare. The sky and shadow settings are under Sky ▾.",
        "灯光沿自身 -Z 方向照射。有灯光的场景由灯光照明；可在视图工具栏切换“摄影棚光”对比。天空和阴影设置在“天空 ▾”里。",
    ),
    ("Temperature", "色温"),
    ("Warm bulb", "暖色灯泡"),
    ("Tungsten film light", "钨丝影视灯"),
    ("Shop fluorescent", "商店荧光灯"),
    ("Daylight, flash", "日光、闪光灯"),
    ("Shade, overcast", "阴影处、阴天"),
    ("Everyday portrait: key a little to the side, soft fill", "日常人像：主光稍偏一侧，柔和补光"),
    ("Butterfly", "蝴蝶光"),
    ("Key high in front, soft fill from below", "主光在正前上方，下方柔和补光"),
    ("Clamshell", "贝壳光"),
    ("Soft light above and below the face: beauty", "脸部上下各一盏柔光：美妆"),
    ("Split", "侧光"),
    ("Hard side light: half lit, half shadow", "硬侧光：一半亮一半暗"),
    ("High key", "高调"),
    ("Bright and even on a white backdrop: catalogue", "白背景上明亮均匀：商品目录"),
    ("Counter", "柜台"),
    ("Shop counter: overhead panels, cool front fill", "商店柜台：顶部灯板，冷色正面补光"),
    ("Low key", "低调"),
    ("One narrow light in the dark: dramatic", "黑暗中一束窄光：戏剧感"),
    ("Window", "窗光"),
    ("Big soft window light from the side", "侧面大面积柔和窗光"),
    ("Neon", "霓虹"),
    ("Magenta and cyan side lights at night", "夜里品红和青色的侧光"),
    ("Stage", "舞台"),
    ("One hard spotlight from above in the dark", "黑暗中从上方打下的一束硬聚光"),
    ("Warm low sun with a sunset sky", "日落天空下的低角度暖阳"),
    ("High sun with a blue sky", "蓝天下的高角度太阳"),
    ("Soft cloudy sky light, no sun", "柔和的阴天天光，没有太阳"),
    ("Moonlight under a night sky", "夜空下的月光"),
    ("Remove all lights and the sky: back to the flat studio light", "移除所有灯光和天空：回到平面摄影棚光"),
    // mesh edit mode
    // boolean
    // mesh, modifiers
    // texture and UVs
    // realistic people
    // other
    // poses
    // hand shapes
    // expressions
    // hair
    // people presets
    // motion presets
    // templates
    // sky, shadows and lighting presets
    // UV editor
    ("UV editor", "UV 编辑器"),
    ("Close the UV editor", "关闭 UV 编辑器"),
    (
        "Open the UV map beside the view: see it over the picture and drag its vertices, edges, faces and islands",
        "在视图旁打开 UV：叠在图片上查看，拖动它的顶点、边、面和孤岛",
    ),
    (
        "Show the UV map beside the view: drag UV vertices, edges, faces and islands over the picture",
        "在视图旁显示 UV：在图片上拖动 UV 顶点、边、面和孤岛",
    ),
    ("Island", "孤岛"),
    ("Align ▾", "对齐 ▾"),
    ("Frame everything (Home)", "全部显示（Home）"),
    ("Weld", "焊接"),
    ("Move the picked corners to one point", "把选中的角移到同一点"),
    ("Stitch", "缝合"),
    (
        "Join each vertex's picked corners at their middle: sews pieces back together along a seam",
        "把每个顶点选中的角合到中点：沿接缝把碎块缝回去",
    ),
    ("Only picked", "只看选中"),
    ("Show only the faces picked in the 3D view (when some are)", "只显示 3D 视图里选中的面（有选中时）"),
    ("Pack", "排列"),
    ("all islands", "所有孤岛"),
    ("the picked faces' islands", "选中面所在的孤岛"),
    ("Sticky", "联动"),
    (
        "Picking a UV vertex or edge also picks the neighbouring faces' corners at the same spot, so the map stays joined",
        "选 UV 顶点或边时，也选中相邻面在同一位置的角，让 UV 保持相连",
    ),
    (
        "Smart cuts it into flat pieces and packs them; Box / Cylinder / Sphere bake that projection.",
        "“智能”会切成平展的小块并排好；盒状 / 圆柱 / 球面会烘焙对应的投影。",
    ),
    ("Snap moves to the picture's pixels (or hold Ctrl)", "移动时吸附到图片像素（或按住 Ctrl）"),
    (
        "This mesh has no UV map yet. Unwrap it to lay its faces flat on the picture:",
        "这个网格还没有 UV。展开它，把面平铺到图片上：",
    ),
    (
        "This mesh has no editable UV map (rigged people are not unwrapped here).",
        "这个网格没有可编辑的 UV（带骨骼的人物不在这里展开）。",
    ),
    ("Unwrap ▾", "展开 UV ▾"),
    ("Use UV map", "改用 UV 贴法"),
    ("The picture is projected, not laid on by this UV map.", "图片现在是投影上去的，没有按这个 UV 贴。"),
    ("Bake a box projection", "烘焙盒状投影"),
    ("Bake a projection from the front", "从正面烘焙投影"),
    ("the whole mesh", "整个网格"),
    ("the picked faces", "选中的面"),
    ("Left", "左"),
    ("Centre (column)", "居中（列）"),
    ("Right", "右"),
    ("Top edge", "上边"),
    ("Middle (row)", "居中（行）"),
    ("Bottom edge", "下边"),
    ("Turn 90° counter-clockwise", "逆时针转 90°"),
    ("Turn 90° clockwise", "顺时针转 90°"),
    ("Mirror left-right", "左右镜像"),
    ("Mirror top-bottom", "上下镜像"),
    (" · U only", " · 只动 U"),
    (" · V only", " · 只动 V"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_duplicates_with_different_wording() {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for (en, zh) in ZH_TABLE {
            if let Some(prev) = seen.insert(en, zh) {
                assert_eq!(prev, *zh, "`{en}` is translated twice differently");
            }
        }
    }
}
