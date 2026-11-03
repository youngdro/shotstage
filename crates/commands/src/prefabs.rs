//! Ready-made, real-size blocks. Each prefab is a group: an empty root at the bottom centre
//! with its parts as children (front = +Z). People are rigged: their joints are empties in a
//! parent chain (pelvis → spine → neck → head, shoulder → elbow → wrist, hip → knee → ankle),
//! and every body part hangs off its joint with the pivot at the joint, so turning a joint
//! bends the limb (see `rig.rs`).

use schemars::JsonSchema;
use serde::Deserialize;
use shotstage_core::{Modifier, Primitive};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum PrefabKind {
    /// Same as `man`.
    Person,
    Man,
    Woman,
    Table,
    Chair,
    Sofa,
    Bed,
    Stairs,
    Wall,
    Doorway,
    Car,
    Tree,
    Lamp,
    Crate,
    ProduceStand,
    AppleCrate,
    OrangeCrate,
    GreensCrate,
    WatermelonBin,
    GondolaShelf,
    OpenChiller,
    IslandFreezer,
    DeliCounter,
    FishDisplay,
    BreadRack,
    Checkout,
    Cart,
    Basket,
    Scale,
    PriceSign,
    Pallet,
    Bottle,
    Jar,
    Can,
    Carton,
    Tube,
    Pouch,
    Cup,
    GiftBox,
    PumpBottle,
    Podium,
    SteppedPodium,
    CubeRiser,
    Pedestal,
    CakeStand,
    Tray,
    AcrylicStand,
    ArchBackdrop,
    InfinityCove,
    PaperRoll,
    ProductTable,
    Softbox,
    Reflector,
    LeafBranch,
    Flower,
    CitrusSlice,
    Pebbles,
    IceCubes,
    Drape,
    Ribbon,
    Splash,
    DecorSpheres,
}

/// Palette group a prefab belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    People,
    Furniture,
    Retail,
    Product,
}

impl Category {
    pub fn key(self) -> &'static str {
        match self {
            Category::People => "people",
            Category::Furniture => "furniture",
            Category::Retail => "retail",
            Category::Product => "product",
        }
    }
}

pub struct PrefabInfo {
    pub kind: PrefabKind,
    /// The `kind` string agents pass.
    pub key: &'static str,
    /// Stem of generated ids (`apple_crate_1`).
    pub stem: &'static str,
    /// Short palette label.
    pub label: &'static str,
    pub description: &'static str,
    pub category: Category,
}

macro_rules! info {
    ($kind:ident, $key:literal, $stem:literal, $label:literal, $cat:ident, $desc:literal) => {
        PrefabInfo {
            kind: PrefabKind::$kind,
            key: $key,
            stem: $stem,
            label: $label,
            description: $desc,
            category: Category::$cat,
        }
    };
}

pub const PREFABS: &[PrefabInfo] = &[
    info!(Man, "man", "man", "Man", People, "Rigged man, 1.76 m, short hair; pose with pose.set, front = +Z"),
    info!(Woman, "woman", "woman", "Woman", People, "Rigged woman, 1.65 m, long hair; pose with pose.set, front = +Z"),
    info!(Person, "person", "person", "Person", People, "Alias of `man`"),
    info!(Table, "table", "table", "Table", Furniture, "Dining table 1.2 × 0.8 m, 0.75 m high"),
    info!(Chair, "chair", "chair", "Chair", Furniture, "Dining chair, seat at 0.45 m"),
    info!(Sofa, "sofa", "sofa", "Sofa", Furniture, "Three-seat sofa, 2 m wide"),
    info!(Bed, "bed", "bed", "Bed", Furniture, "Double bed 1.6 × 2.1 m"),
    info!(Lamp, "lamp", "lamp", "Lamp", Furniture, "Floor lamp, 1.75 m"),
    info!(Crate, "crate", "crate", "Box", Furniture, "Box 0.6 m"),
    info!(Stairs, "stairs", "stairs", "Stairs", Furniture, "Straight stairs, 10 steps of 18 cm"),
    info!(Wall, "wall", "wall", "Wall", Furniture, "Wall 4 m wide, 2.6 m high"),
    info!(Doorway, "doorway", "doorway", "Doorway", Furniture, "Wall 4 m wide with a 0.9 × 2.1 m opening"),
    info!(Car, "car", "car", "Car", Furniture, "Car 4.4 m long"),
    info!(Tree, "tree", "tree", "Tree", Furniture, "Tree, about 3.9 m tall"),
    info!(
        ProduceStand,
        "produceStand",
        "produce_stand",
        "Produce",
        Retail,
        "Two-tier produce stand 1.6 × 1.0 m with crates of apples, oranges and greens"
    ),
    info!(AppleCrate, "appleCrate", "apple_crate", "Apples", Retail, "Crate of red apples 0.6 × 0.4 m"),
    info!(OrangeCrate, "orangeCrate", "orange_crate", "Oranges", Retail, "Crate of oranges 0.6 × 0.4 m"),
    info!(
        GreensCrate,
        "greensCrate", "greens_crate", "Greens", Retail, "Crate of leafy greens and cucumbers 0.6 × 0.4 m"
    ),
    info!(WatermelonBin, "watermelonBin", "watermelon_bin", "Melons", Retail, "Cardboard bin of watermelons 1 × 1 m"),
    info!(
        GondolaShelf,
        "gondolaShelf", "shelf", "Shelf", Retail, "Gondola shelf 1.2 m wide, 1.8 m high, stocked with packs"
    ),
    info!(
        OpenChiller,
        "openChiller", "chiller", "Chiller", Retail, "Open multideck chiller 1.25 m wide, 2.1 m high, with bottles"
    ),
    info!(
        IslandFreezer,
        "islandFreezer", "freezer", "Freezer", Retail, "Island chest freezer 2 × 0.9 m with a glass lid"
    ),
    info!(
        DeliCounter,
        "deliCounter",
        "deli_counter",
        "Deli",
        Retail,
        "Serve-over meat / deli counter 2 m wide with a sloped glass front"
    ),
    info!(
        FishDisplay,
        "fishDisplay", "fish_display", "Fish", Retail, "Fish-on-ice display 1.8 m wide, tilted to the customer"
    ),
    info!(
        BreadRack,
        "breadRack", "bread_rack", "Bread", Retail, "Bakery rack 1.2 m wide with sloped shelves of loaves"
    ),
    info!(
        Checkout,
        "checkout",
        "checkout",
        "Checkout",
        Retail,
        "Checkout counter 1.8 m with belt, screen and card reader; cashier stands at -Z"
    ),
    info!(Cart, "cart", "cart", "Cart", Retail, "Shopping trolley, handle at -Z"),
    info!(Basket, "basket", "basket", "Basket", Retail, "Hand basket 0.45 × 0.3 m"),
    info!(Scale, "scale", "scale", "Scale", Retail, "Produce weighing scale with display"),
    info!(PriceSign, "priceSign", "price_sign", "Sign", Retail, "Price sign on a stand, 1.3 m"),
    info!(Pallet, "pallet", "pallet", "Pallet", Retail, "Pallet 1.2 × 1.0 m stacked with cartons"),
    info!(
        Bottle,
        "bottle",
        "bottle",
        "Bottle",
        Product,
        "Hero bottle stand-in 7 cm × 25 cm (use `size` to match the real product)"
    ),
    info!(Jar, "jar", "jar", "Jar", Product, "Jar with lid, 8 cm × 10 cm"),
    info!(Can, "can", "can", "Can", Product, "Drinks can 6.6 cm × 12 cm"),
    info!(Carton, "carton", "carton", "Carton", Product, "Product box / carton 12 × 18 × 5 cm"),
    info!(Tube, "tube", "tube", "Tube", Product, "Cosmetics tube standing on its cap, 16 cm"),
    info!(Pouch, "pouch", "pouch", "Pouch", Product, "Stand-up pouch 14 × 20 cm"),
    info!(Cup, "cup", "cup", "Cup", Product, "Coffee cup with lid, 12 cm"),
    info!(GiftBox, "giftBox", "gift_box", "Gift box", Product, "Gift box with ribbon 20 cm"),
    info!(PumpBottle, "pumpBottle", "pump_bottle", "Pump", Product, "Cosmetics bottle with pump, 18 cm"),
    info!(Podium, "podium", "podium", "Podium", Product, "Round plinth 40 cm across, 20 cm high"),
    info!(
        SteppedPodium,
        "steppedPodium", "stepped_podium", "Steps", Product, "Two-step round podium for a product line-up"
    ),
    info!(CubeRiser, "cubeRiser", "riser", "Riser", Product, "Cube riser 20 cm"),
    info!(Pedestal, "pedestal", "pedestal", "Pedestal", Product, "Column pedestal 30 cm across, 90 cm high"),
    info!(CakeStand, "cakeStand", "cake_stand", "Cake stand", Product, "Footed cake stand 28 cm"),
    info!(Tray, "tray", "tray", "Tray", Product, "Serving tray 40 × 28 cm"),
    info!(AcrylicStand, "acrylicStand", "acrylic_stand", "Acrylic", Product, "Clear acrylic riser 30 × 15 cm"),
    info!(ArchBackdrop, "archBackdrop", "arch", "Arch", Product, "Arched backdrop panel 1.2 m wide, 2.2 m high"),
    info!(
        InfinityCove,
        "infinityCove", "cove", "Cove", Product, "Seamless infinity cove: 4 m wide floor curving up into a 3 m wall"
    ),
    info!(
        PaperRoll,
        "paperRoll", "paper_roll", "Paper roll", Product, "Backdrop paper roll on stands, 2.7 m wide sweep"
    ),
    info!(
        ProductTable,
        "productTable", "product_table", "Shoot table", Product, "Tabletop for product shots 1.0 × 0.7 m, 0.75 m high"
    ),
    info!(
        Softbox,
        "softbox",
        "softbox",
        "Softbox",
        Product,
        "Softbox on a light stand, head at 1.8 m, aimed forward (+Z); it is a real area light"
    ),
    info!(Reflector, "reflector", "reflector", "Reflector", Product, "Round reflector card 80 cm on a stand"),
    info!(LeafBranch, "leafBranch", "branch", "Leaves", Product, "Small branch with leaves, 35 cm"),
    info!(Flower, "flower", "flower", "Flower", Product, "Single flower on a stem, 30 cm"),
    info!(CitrusSlice, "citrusSlice", "slice", "Slice", Product, "Orange slice 7 cm"),
    info!(Pebbles, "pebbles", "pebbles", "Pebbles", Product, "A few smooth stones"),
    info!(IceCubes, "iceCubes", "ice", "Ice", Product, "Scattered ice cubes"),
    info!(Drape, "drape", "drape", "Drape", Product, "Folded fabric drape 1 × 0.6 m"),
    info!(Ribbon, "ribbon", "ribbon", "Ribbon", Product, "Ribbon loop 30 cm"),
    info!(Splash, "splash", "splash", "Splash", Product, "Water splash stand-in (crown and droplets), 25 cm"),
    info!(DecorSpheres, "decorSpheres", "spheres", "Spheres", Product, "Three decor spheres of different sizes"),
];

pub fn info(kind: PrefabKind) -> &'static PrefabInfo {
    PREFABS.iter().find(|p| p.kind == kind).expect("every kind is listed")
}

/// One part of a prefab.
pub struct Part {
    pub name: &'static str,
    /// Parent part name; `None` = the group root.
    pub parent: Option<&'static str>,
    /// `None` makes an empty (a joint of a rig).
    pub prim: Option<Primitive>,
    pub pos: [f32; 3],
    /// Degrees `[pitchX, yawY, rollZ]`.
    pub rot: [f32; 3],
    pub scale: [f32; 3],
    pub color: [f32; 3],
    pub modifiers: Vec<Modifier>,
    /// Makes the part a light (`prim` is then ignored).
    pub light: Option<shotstage_core::LightData>,
}

pub fn part(name: &'static str, prim: Primitive, pos: [f32; 3], color: [f32; 3]) -> Part {
    Part {
        name,
        parent: None,
        prim: Some(prim),
        pos,
        rot: [0.0; 3],
        scale: [1.0; 3],
        color,
        modifiers: vec![],
        light: None,
    }
}

fn cube(x: f32, y: f32, z: f32) -> Primitive {
    Primitive::Cube { size: [x, y, z] }
}

fn cyl(r: f32, h: f32) -> Primitive {
    Primitive::Cylinder { radius: r, height: h, segments: 24 }
}

/// A unit sphere; scale the part to get an ellipsoid of that size.
fn ball(segments: u32) -> Primitive {
    Primitive::Sphere { radius: 0.5, segments, rings: (segments / 2).max(4) }
}

fn array(count: u32, offset: [f32; 3]) -> Modifier {
    Modifier::Array { count, offset }
}

const WOOD: [f32; 3] = [0.62, 0.46, 0.32];
const FABRIC: [f32; 3] = [0.42, 0.5, 0.62];
const WALL: [f32; 3] = [0.8, 0.78, 0.74];
const METAL: [f32; 3] = [0.35, 0.36, 0.4];
const STEEL: [f32; 3] = [0.72, 0.74, 0.77];
const WHITE: [f32; 3] = [0.9, 0.91, 0.92];
const GLASS: [f32; 3] = [0.7, 0.82, 0.88];
const DARK: [f32; 3] = [0.16, 0.17, 0.19];
const CARDBOARD: [f32; 3] = [0.7, 0.55, 0.36];
const APPLE: [f32; 3] = [0.78, 0.13, 0.12];
const ORANGE: [f32; 3] = [0.95, 0.55, 0.12];
const GREEN: [f32; 3] = [0.3, 0.58, 0.22];
const LIGHT_GREEN: [f32; 3] = [0.55, 0.75, 0.3];
const MELON: [f32; 3] = [0.2, 0.42, 0.18];
const BREAD: [f32; 3] = [0.76, 0.52, 0.28];
const MEAT: [f32; 3] = [0.72, 0.25, 0.25];
const FISH: [f32; 3] = [0.6, 0.66, 0.72];
const RED: [f32; 3] = [0.75, 0.15, 0.15];
const GOLD: [f32; 3] = [0.85, 0.7, 0.35];
const PASTEL: [f32; 3] = [0.93, 0.86, 0.8];
const FABRIC_LIGHT: [f32; 3] = [0.9, 0.86, 0.8];
const WATER: [f32; 3] = [0.75, 0.88, 0.95];

/// `nx × nz` fruits of radius `r` (as one mesh with two Array modifiers) centred at `center`
/// (y = the fruit centres), `gap` metres apart.
fn fruit(name: &'static str, r: f32, nx: u32, nz: u32, center: [f32; 3], color: [f32; 3]) -> Part {
    let s = r * 2.05;
    let start = [center[0] - (nx - 1) as f32 * s / 2.0, center[1], center[2] - (nz - 1) as f32 * s / 2.0];
    Part {
        modifiers: vec![array(nx, [s, 0.0, 0.0]), array(nz, [0.0, 0.0, s])],
        ..part(name, Primitive::Sphere { radius: r, segments: 10, rings: 6 }, start, color)
    }
}

/// An open wooden crate (bottom centre at `base`) with `n` produce rows in it.
fn crate_with(
    prefix: [&'static str; 2],
    base: [f32; 3],
    size: [f32; 2],
    produce: Option<(f32, [f32; 3])>,
) -> Vec<Part> {
    let [w, d] = size;
    let h = 0.12;
    let mut v = vec![part(prefix[0], cube(w, h, d), [base[0], base[1] + h / 2.0, base[2]], WOOD)];
    if let Some((r, color)) = produce {
        let nx = ((w - 0.04) / (r * 2.05)).floor().max(1.0) as u32;
        let nz = ((d - 0.04) / (r * 2.05)).floor().max(1.0) as u32;
        v.push(fruit(prefix[1], r, nx, nz, [base[0], base[1] + h + r * 0.6, base[2]], color));
    }
    v
}

fn legs4(names: [&'static str; 4], w: f32, d: f32, h: f32, t: f32, color: [f32; 3]) -> Vec<Part> {
    let (x, z) = (w / 2.0 - t / 2.0, d / 2.0 - t / 2.0);
    names
        .iter()
        .zip([(x, z), (-x, z), (x, -z), (-x, -z)])
        .map(|(n, (px, pz))| part(n, cube(t, h, t), [px, h / 2.0, pz], color))
        .collect()
}

/// Prefab parts with real-world sizes, bottom centre at the origin, front facing +Z.
pub fn parts(kind: PrefabKind) -> Vec<Part> {
    use PrefabKind::*;
    match kind {
        Person | Man => crate::rig::person(crate::rig::Body::Man),
        Woman => crate::rig::person(crate::rig::Body::Woman),
        Table => {
            let mut v = vec![part("top", cube(1.2, 0.04, 0.8), [0.0, 0.73, 0.0], WOOD)];
            v.extend(legs4(["leg_1", "leg_2", "leg_3", "leg_4"], 1.1, 0.7, 0.71, 0.05, WOOD));
            v
        }
        Chair => {
            let mut v = vec![
                part("seat", cube(0.45, 0.05, 0.45), [0.0, 0.45, 0.0], WOOD),
                part("back", cube(0.45, 0.5, 0.04), [0.0, 0.72, -0.205], WOOD),
            ];
            v.extend(legs4(["leg_1", "leg_2", "leg_3", "leg_4"], 0.42, 0.42, 0.43, 0.04, WOOD));
            v
        }
        Sofa => vec![
            part("base", cube(2.0, 0.42, 0.9), [0.0, 0.21, 0.0], FABRIC),
            part("back", cube(2.0, 0.45, 0.2), [0.0, 0.645, -0.35], FABRIC),
            part("arm_l", cube(0.2, 0.62, 0.9), [0.9, 0.31, 0.0], FABRIC),
            part("arm_r", cube(0.2, 0.62, 0.9), [-0.9, 0.31, 0.0], FABRIC),
        ],
        Bed => vec![
            part("frame", cube(1.6, 0.3, 2.1), [0.0, 0.15, 0.0], WOOD),
            part("mattress", cube(1.5, 0.22, 2.0), [0.0, 0.41, 0.02], [0.92, 0.92, 0.9]),
            part("headboard", cube(1.6, 0.7, 0.08), [0.0, 0.65, -1.01], WOOD),
        ],
        Stairs => vec![Part {
            modifiers: vec![array(10, [0.0, 0.18, -0.28])],
            ..part("steps", cube(1.0, 0.18, 0.28), [0.0, 0.09, 0.0], WALL)
        }],
        Wall => vec![part("wall", cube(4.0, 2.6, 0.12), [0.0, 1.3, 0.0], WALL)],
        Doorway => vec![
            part("left", cube(1.55, 2.6, 0.12), [1.225, 1.3, 0.0], WALL),
            part("right", cube(1.55, 2.6, 0.12), [-1.225, 1.3, 0.0], WALL),
            part("lintel", cube(0.9, 0.5, 0.12), [0.0, 2.35, 0.0], WALL),
        ],
        Car => {
            let mut v = vec![
                part("body", cube(1.8, 0.7, 4.4), [0.0, 0.6, 0.0], [0.7, 0.2, 0.18]),
                part("cabin", cube(1.6, 0.55, 2.2), [0.0, 1.225, -0.25], [0.55, 0.62, 0.7]),
            ];
            for (n, x, z) in
                [("wheel_1", 0.85, 1.4), ("wheel_2", -0.85, 1.4), ("wheel_3", 0.85, -1.4), ("wheel_4", -0.85, -1.4)]
            {
                v.push(Part { rot: [0.0, 0.0, 90.0], ..part(n, cyl(0.33, 0.25), [x, 0.33, z], [0.12, 0.12, 0.13]) });
            }
            v
        }
        Tree => vec![
            part("trunk", cyl(0.15, 2.0), [0.0, 1.0, 0.0], [0.45, 0.32, 0.22]),
            part(
                "crown",
                Primitive::Sphere { radius: 1.1, segments: 20, rings: 12 },
                [0.0, 2.8, 0.0],
                [0.36, 0.56, 0.3],
            ),
        ],
        Lamp => vec![
            part("base", cyl(0.15, 0.03), [0.0, 0.015, 0.0], METAL),
            part("pole", cyl(0.02, 1.5), [0.0, 0.78, 0.0], METAL),
            part(
                "shade",
                Primitive::Cone { radius: 0.22, height: 0.3, segments: 24 },
                [0.0, 1.6, 0.0],
                [0.95, 0.9, 0.7],
            ),
        ],
        Crate => vec![part("crate", cube(0.6, 0.6, 0.6), [0.0, 0.3, 0.0], WOOD)],

        // ------------------------------------------------------------ fresh retail
        ProduceStand => {
            let mut v = vec![
                part("base", cube(1.6, 0.7, 1.0), [0.0, 0.35, 0.0], WOOD),
                part("riser", cube(1.6, 0.25, 0.5), [0.0, 0.825, -0.25], WOOD),
            ];
            v.extend(crate_with(["crate_1", "apples"], [-0.4, 0.7, 0.25], [0.75, 0.45], Some((0.04, APPLE))));
            v.extend(crate_with(["crate_2", "oranges"], [0.4, 0.7, 0.25], [0.75, 0.45], Some((0.042, ORANGE))));
            v.extend(crate_with(["crate_3", "greens"], [-0.4, 0.95, -0.25], [0.75, 0.45], Some((0.05, GREEN))));
            v.extend(crate_with(
                ["crate_4", "lemons"],
                [0.4, 0.95, -0.25],
                [0.75, 0.45],
                Some((0.035, [0.95, 0.85, 0.25])),
            ));
            v
        }
        AppleCrate => crate_with(["crate", "apples"], [0.0; 3], [0.6, 0.4], Some((0.04, APPLE))),
        OrangeCrate => crate_with(["crate", "oranges"], [0.0; 3], [0.6, 0.4], Some((0.042, ORANGE))),
        GreensCrate => {
            let mut v = crate_with(["crate", "lettuce"], [0.0; 3], [0.6, 0.4], None);
            v.push(Part { scale: [1.0, 0.75, 1.0], ..fruit("lettuce", 0.07, 2, 2, [-0.14, 0.17, 0.0], GREEN) });
            v.push(Part {
                rot: [90.0, 0.0, 0.0],
                modifiers: vec![array(4, [0.05, 0.0, 0.0])],
                ..part(
                    "cucumbers",
                    Primitive::Capsule { radius: 0.022, height: 0.24, segments: 8 },
                    [0.07, 0.145, 0.0],
                    LIGHT_GREEN,
                )
            });
            v
        }
        WatermelonBin => vec![
            part("bin", cube(1.0, 0.55, 1.0), [0.0, 0.275, 0.0], CARDBOARD),
            // array offsets are measured before the part's scale, so divide by it
            Part {
                scale: [0.28, 0.24, 0.35],
                modifiers: vec![array(3, [0.3 / 0.28, 0.0, 0.0]), array(3, [0.0, 0.0, 0.3 / 0.35])],
                ..part("melons", ball(12), [-0.3, 0.62, -0.3], MELON)
            },
        ],
        GondolaShelf => {
            let mut v = vec![
                part("base", cube(1.2, 0.12, 0.5), [0.0, 0.06, 0.0], STEEL),
                part("back", cube(1.2, 1.8, 0.05), [0.0, 0.9, -0.225], STEEL),
            ];
            let packs = [[0.85, 0.3, 0.25], [0.25, 0.45, 0.75], [0.95, 0.75, 0.2], [0.3, 0.6, 0.35]];
            for (i, (shelf, pack)) in
                [("shelf_1", "packs_1"), ("shelf_2", "packs_2"), ("shelf_3", "packs_3"), ("shelf_4", "packs_4")]
                    .into_iter()
                    .enumerate()
            {
                let y = 0.12 + i as f32 * 0.42;
                if i > 0 {
                    v.push(part(shelf, cube(1.2, 0.03, 0.45), [0.0, y, 0.025], STEEL));
                }
                v.push(Part {
                    modifiers: vec![array(12, [0.095, 0.0, 0.0]), array(2, [0.0, 0.0, -0.15])],
                    ..part(pack, cube(0.08, 0.22, 0.12), [-0.5225, y + 0.125, 0.15], packs[i])
                });
            }
            v
        }
        OpenChiller => {
            let mut v = vec![
                part("base", cube(1.25, 0.5, 0.9), [0.0, 0.25, 0.0], WHITE),
                part("back", cube(1.25, 2.1, 0.15), [0.0, 1.05, -0.375], WHITE),
                part("side_l", cube(0.05, 2.1, 0.9), [0.6, 1.05, 0.0], WHITE),
                part("side_r", cube(0.05, 2.1, 0.9), [-0.6, 1.05, 0.0], WHITE),
                part("canopy", cube(1.25, 0.2, 0.75), [0.0, 2.0, -0.075], WHITE),
                part("inside", cube(1.15, 1.4, 0.02), [0.0, 1.2, -0.29], DARK),
            ];
            let colors = [[0.85, 0.85, 0.9], [0.95, 0.75, 0.2], [0.55, 0.75, 0.95], [0.85, 0.25, 0.25]];
            for (i, (shelf, row)) in
                [("shelf_1", "bottles_1"), ("shelf_2", "bottles_2"), ("shelf_3", "bottles_3"), ("shelf_4", "bottles_4")]
                    .into_iter()
                    .enumerate()
            {
                let y = 0.5 + i as f32 * 0.37;
                let depth = 0.6 - i as f32 * 0.06;
                v.push(part(shelf, cube(1.15, 0.03, depth), [0.0, y, -0.28 + depth / 2.0], STEEL));
                v.push(Part {
                    modifiers: vec![array(14, [0.08, 0.0, 0.0]), array(2, [0.0, 0.0, -0.12])],
                    ..part(
                        row,
                        Primitive::Cylinder { radius: 0.033, height: 0.24, segments: 10 },
                        [-0.52, y + 0.135, -0.28 + depth - 0.08],
                        colors[i],
                    )
                });
            }
            v
        }
        IslandFreezer => vec![
            part("body", cube(2.0, 0.85, 0.9), [0.0, 0.425, 0.0], WHITE),
            part("bumper", cube(2.04, 0.12, 0.94), [0.0, 0.15, 0.0], [0.3, 0.45, 0.7]),
            part("lid", cube(1.9, 0.02, 0.8), [0.0, 0.86, 0.0], GLASS),
            part("divider", cube(0.03, 0.04, 0.8), [0.0, 0.89, 0.0], STEEL),
        ],
        DeliCounter => {
            let mut v = vec![
                part("base", cube(2.0, 0.85, 0.9), [0.0, 0.425, 0.0], STEEL),
                part("deck", cube(1.9, 0.04, 0.6), [0.0, 0.87, 0.05], WHITE),
                part("rear_shelf", cube(2.0, 0.04, 0.3), [0.0, 1.32, -0.3], STEEL),
                part("post_l", cube(0.04, 0.45, 0.04), [0.98, 1.1, -0.3], STEEL),
                part("post_r", cube(0.04, 0.45, 0.04), [-0.98, 1.1, -0.3], STEEL),
                Part { rot: [-35.0, 0.0, 0.0], ..part("glass", cube(2.0, 0.55, 0.015), [0.0, 1.1, 0.3], GLASS) },
            ];
            v.push(Part {
                modifiers: vec![array(8, [0.22, 0.0, 0.0]), array(2, [0.0, 0.0, -0.22])],
                ..part("cuts", cube(0.18, 0.05, 0.16), [-0.77, 0.915, 0.2], MEAT)
            });
            v
        }
        FishDisplay => vec![
            part("base", cube(1.8, 0.75, 1.0), [0.0, 0.375, 0.0], STEEL),
            Part { rot: [12.0, 0.0, 0.0], ..part("ice", cube(1.7, 0.12, 0.9), [0.0, 0.85, 0.0], WHITE) },
            Part {
                rot: [12.0, 0.0, 0.0],
                scale: [0.36, 0.06, 0.11],
                modifiers: vec![array(4, [0.42 / 0.36, 0.0, 0.0]), array(5, [0.0, 0.0, 0.17 / 0.11])],
                ..part("fish", ball(10), [-0.63, 0.94, -0.34], FISH)
            },
        ],
        BreadRack => {
            let mut v = vec![
                part("post_l", cube(0.04, 1.6, 0.5), [0.58, 0.8, 0.0], WOOD),
                part("post_r", cube(0.04, 1.6, 0.5), [-0.58, 0.8, 0.0], WOOD),
            ];
            for (i, (shelf, loaves)) in
                [("shelf_1", "loaves_1"), ("shelf_2", "loaves_2"), ("shelf_3", "loaves_3")].into_iter().enumerate()
            {
                let y = 0.35 + i as f32 * 0.45;
                v.push(Part { rot: [10.0, 0.0, 0.0], ..part(shelf, cube(1.12, 0.03, 0.48), [0.0, y, 0.0], WOOD) });
                v.push(Part {
                    rot: [10.0, 0.0, 0.0],
                    scale: [0.12, 0.09, 0.26],
                    modifiers: vec![array(7, [0.15 / 0.12, 0.0, 0.0])],
                    ..part(loaves, ball(10), [-0.45, y + 0.06, 0.0], BREAD)
                });
            }
            v
        }
        Checkout => vec![
            part("counter", cube(1.8, 0.88, 0.6), [0.0, 0.44, 0.0], WHITE),
            part("belt", cube(1.1, 0.02, 0.42), [0.3, 0.89, 0.0], DARK),
            part("pole", cube(0.04, 0.3, 0.04), [-0.55, 1.03, -0.15], METAL),
            Part { rot: [-15.0, 180.0, 0.0], ..part("screen", cube(0.32, 0.22, 0.04), [-0.55, 1.25, -0.15], DARK) },
            part("reader", cube(0.08, 0.12, 0.06), [-0.55, 0.94, 0.18], DARK),
            part("bag_shelf", cube(0.5, 0.04, 0.5), [-0.7, 0.75, 0.05], STEEL),
        ],
        Cart => {
            let mut v = vec![
                part("floor", cube(0.5, 0.02, 0.85), [0.0, 0.55, 0.0], STEEL),
                part("side_l", cube(0.02, 0.4, 0.85), [0.25, 0.75, 0.0], STEEL),
                part("side_r", cube(0.02, 0.4, 0.85), [-0.25, 0.75, 0.0], STEEL),
                part("front", cube(0.5, 0.4, 0.02), [0.0, 0.75, 0.425], STEEL),
                part("back", cube(0.5, 0.4, 0.02), [0.0, 0.75, -0.425], STEEL),
                Part { rot: [0.0, 0.0, 90.0], ..part("handle", cyl(0.018, 0.56), [0.0, 1.0, -0.5], RED) },
                part("handle_l", cube(0.02, 0.1, 0.1), [0.26, 0.97, -0.46], STEEL),
                part("handle_r", cube(0.02, 0.1, 0.1), [-0.26, 0.97, -0.46], STEEL),
                part("chassis", cube(0.44, 0.03, 0.8), [0.0, 0.12, 0.0], STEEL),
                part("leg_back", cube(0.44, 0.45, 0.03), [0.0, 0.34, -0.38], STEEL),
            ];
            for (n, x, z) in
                [("wheel_1", 0.2, 0.36), ("wheel_2", -0.2, 0.36), ("wheel_3", 0.2, -0.36), ("wheel_4", -0.2, -0.36)]
            {
                v.push(Part { rot: [0.0, 0.0, 90.0], ..part(n, cyl(0.05, 0.03), [x, 0.05, z], DARK) });
            }
            v
        }
        Basket => vec![
            part("bottom", cube(0.45, 0.02, 0.3), [0.0, 0.01, 0.0], RED),
            part("side_l", cube(0.02, 0.22, 0.3), [0.215, 0.12, 0.0], RED),
            part("side_r", cube(0.02, 0.22, 0.3), [-0.215, 0.12, 0.0], RED),
            part("front", cube(0.45, 0.22, 0.02), [0.0, 0.12, 0.14], RED),
            part("back", cube(0.45, 0.22, 0.02), [0.0, 0.12, -0.14], RED),
            Part {
                rot: [90.0, 0.0, 0.0],
                scale: [1.0, 1.0, 0.6],
                ..part(
                    "handle",
                    Primitive::Torus { major_radius: 0.17, minor_radius: 0.01, major_segments: 24, minor_segments: 6 },
                    [0.0, 0.23, 0.0],
                    DARK,
                )
            },
        ],
        Scale => vec![
            part("body", cube(0.35, 0.1, 0.35), [0.0, 0.05, 0.0], WHITE),
            part("platter", cube(0.32, 0.02, 0.3), [0.0, 0.11, 0.02], STEEL),
            part("post", cube(0.04, 0.22, 0.04), [0.0, 0.21, -0.15], WHITE),
            Part { rot: [0.0, 0.0, 0.0], ..part("display", cube(0.22, 0.08, 0.04), [0.0, 0.34, -0.15], DARK) },
        ],
        PriceSign => vec![
            part("foot", cyl(0.18, 0.03), [0.0, 0.015, 0.0], METAL),
            part("pole", cyl(0.015, 1.0), [0.0, 0.53, 0.0], METAL),
            part("board", cube(0.6, 0.4, 0.02), [0.0, 1.15, 0.0], [0.95, 0.85, 0.3]),
            part("label", cube(0.5, 0.12, 0.022), [0.0, 1.22, 0.0], RED),
        ],
        Pallet => vec![
            part("deck", cube(1.2, 0.04, 1.0), [0.0, 0.124, 0.0], WOOD),
            Part {
                modifiers: vec![array(3, [0.0, 0.0, -0.45])],
                ..part("runners", cube(1.2, 0.1, 0.1), [0.0, 0.05, 0.45], WOOD)
            },
            Part {
                modifiers: vec![array(3, [0.4, 0.0, 0.0]), array(3, [0.0, 0.0, -0.33]), array(2, [0.0, 0.3, 0.0])],
                ..part("cartons", cube(0.39, 0.29, 0.32), [-0.4, 0.29, 0.33], CARDBOARD)
            },
        ],
        // ------------------------------------------------------------ product shoot
        Bottle => vec![
            part("body", cyl(0.035, 0.19), [0.0, 0.095, 0.0], [0.25, 0.5, 0.35]),
            part(
                "shoulder",
                Primitive::Cone { radius: 0.035, height: 0.04, segments: 24 },
                [0.0, 0.21, 0.0],
                [0.25, 0.5, 0.35],
            ),
            part("neck", cyl(0.013, 0.03), [0.0, 0.235, 0.0], [0.25, 0.5, 0.35]),
            part("cap", cyl(0.015, 0.02), [0.0, 0.26, 0.0], GOLD),
            part("label", cyl(0.0355, 0.08), [0.0, 0.1, 0.0], WHITE),
        ],
        Jar => vec![
            part("body", cyl(0.04, 0.085), [0.0, 0.0425, 0.0], GLASS),
            part("lid", cyl(0.042, 0.02), [0.0, 0.095, 0.0], GOLD),
        ],
        Can => vec![
            part("body", cyl(0.033, 0.115), [0.0, 0.0575, 0.0], [0.8, 0.2, 0.25]),
            part("top", cyl(0.029, 0.006), [0.0, 0.118, 0.0], STEEL),
        ],
        Carton => vec![part("box", cube(0.12, 0.18, 0.05), [0.0, 0.09, 0.0], [0.9, 0.85, 0.75])],
        Tube => vec![
            part("cap", cyl(0.018, 0.025), [0.0, 0.0125, 0.0], WHITE),
            Part {
                scale: [1.0, 1.0, 0.45],
                ..part(
                    "body",
                    Primitive::Cone { radius: 0.03, height: 0.14, segments: 20 },
                    [0.0, 0.095, 0.0],
                    [0.55, 0.75, 0.85],
                )
            },
            Part {
                scale: [1.0, 1.0, 0.1],
                ..part("crimp", cube(0.062, 0.012, 0.03), [0.0, 0.168, 0.0], [0.55, 0.75, 0.85])
            },
        ],
        Pouch => vec![
            Part { scale: [0.14, 0.2, 0.05], ..part("body", ball(16), [0.0, 0.1, 0.0], [0.95, 0.75, 0.3]) },
            part("base", cube(0.12, 0.02, 0.05), [0.0, 0.01, 0.0], [0.95, 0.75, 0.3]),
            part("seal", cube(0.13, 0.015, 0.012), [0.0, 0.195, 0.0], [0.95, 0.75, 0.3]),
        ],
        Cup => vec![
            Part {
                rot: [180.0, 0.0, 0.0],
                ..part("body", Primitive::Cone { radius: 0.045, height: 0.11, segments: 24 }, [0.0, 0.055, 0.0], WHITE)
            },
            part("sleeve", cyl(0.041, 0.04), [0.0, 0.06, 0.0], CARDBOARD),
            part("lid", cyl(0.047, 0.015), [0.0, 0.115, 0.0], DARK),
        ],
        GiftBox => vec![
            part("box", cube(0.2, 0.14, 0.2), [0.0, 0.07, 0.0], [0.85, 0.2, 0.25]),
            part("lid", cube(0.21, 0.03, 0.21), [0.0, 0.15, 0.0], [0.85, 0.2, 0.25]),
            part("band_x", cube(0.215, 0.172, 0.03), [0.0, 0.083, 0.0], GOLD),
            part("band_z", cube(0.03, 0.172, 0.215), [0.0, 0.083, 0.0], GOLD),
            Part {
                rot: [0.0, 45.0, 0.0],
                scale: [1.0, 1.0, 0.4],
                ..part(
                    "bow",
                    Primitive::Torus { major_radius: 0.04, minor_radius: 0.012, major_segments: 16, minor_segments: 6 },
                    [0.0, 0.18, 0.0],
                    GOLD,
                )
            },
        ],
        PumpBottle => vec![
            part("body", cyl(0.032, 0.13), [0.0, 0.065, 0.0], [0.92, 0.88, 0.84]),
            part("collar", cyl(0.016, 0.02), [0.0, 0.14, 0.0], GOLD),
            part("stem", cyl(0.005, 0.025), [0.0, 0.162, 0.0], WHITE),
            part("head", cube(0.02, 0.015, 0.05), [0.0, 0.18, 0.012], WHITE),
        ],
        Podium => vec![part(
            "podium",
            Primitive::Cylinder { radius: 0.2, height: 0.2, segments: 48 },
            [0.0, 0.1, 0.0],
            PASTEL,
        )],
        SteppedPodium => vec![
            part("lower", Primitive::Cylinder { radius: 0.32, height: 0.12, segments: 48 }, [0.0, 0.06, 0.0], PASTEL),
            part(
                "upper",
                Primitive::Cylinder { radius: 0.18, height: 0.12, segments: 48 },
                [-0.08, 0.18, -0.06],
                PASTEL,
            ),
        ],
        CubeRiser => vec![part("riser", cube(0.2, 0.2, 0.2), [0.0, 0.1, 0.0], PASTEL)],
        Pedestal => vec![
            part("plinth", cube(0.36, 0.06, 0.36), [0.0, 0.03, 0.0], WHITE),
            part("column", Primitive::Cylinder { radius: 0.15, height: 0.78, segments: 32 }, [0.0, 0.45, 0.0], WHITE),
            part("cap", cube(0.36, 0.06, 0.36), [0.0, 0.87, 0.0], WHITE),
        ],
        CakeStand => vec![
            part("foot", Primitive::Cylinder { radius: 0.06, height: 0.015, segments: 32 }, [0.0, 0.0075, 0.0], WHITE),
            part("stem", cyl(0.012, 0.08), [0.0, 0.055, 0.0], WHITE),
            part("plate", Primitive::Cylinder { radius: 0.14, height: 0.012, segments: 48 }, [0.0, 0.1, 0.0], WHITE),
        ],
        Tray => vec![
            part("base", cube(0.4, 0.01, 0.28), [0.0, 0.005, 0.0], WOOD),
            part("rim_f", cube(0.4, 0.03, 0.01), [0.0, 0.015, 0.135], WOOD),
            part("rim_b", cube(0.4, 0.03, 0.01), [0.0, 0.015, -0.135], WOOD),
            part("rim_l", cube(0.01, 0.03, 0.28), [0.195, 0.015, 0.0], WOOD),
            part("rim_r", cube(0.01, 0.03, 0.28), [-0.195, 0.015, 0.0], WOOD),
        ],
        AcrylicStand => vec![
            part("top", cube(0.3, 0.01, 0.15), [0.0, 0.115, 0.0], GLASS),
            part("side_l", cube(0.01, 0.11, 0.15), [0.145, 0.055, 0.0], GLASS),
            part("side_r", cube(0.01, 0.11, 0.15), [-0.145, 0.055, 0.0], GLASS),
        ],
        ArchBackdrop => vec![
            part("panel", cube(1.2, 1.6, 0.05), [0.0, 0.8, 0.0], PASTEL),
            Part {
                rot: [90.0, 0.0, 0.0],
                ..part("arch", Primitive::Cylinder { radius: 0.6, height: 0.05, segments: 48 }, [0.0, 1.6, 0.0], PASTEL)
            },
        ],
        InfinityCove => vec![part(
            "sweep",
            Primitive::Cove { size: [4.0, 3.0, 4.0], radius: 0.8 },
            [0.0, 1.5, 0.0],
            [0.92, 0.92, 0.9],
        )],
        PaperRoll => vec![
            part("sweep", Primitive::Cove { size: [2.7, 2.6, 2.2], radius: 0.6 }, [0.0, 1.3, 0.0], [0.82, 0.86, 0.9]),
            Part { rot: [0.0, 0.0, 90.0], ..part("roll", cyl(0.06, 2.75), [0.0, 2.65, -1.1], [0.82, 0.86, 0.9]) },
            part("stand_l", cyl(0.015, 2.7), [1.42, 1.35, -1.1], METAL),
            part("stand_r", cyl(0.015, 2.7), [-1.42, 1.35, -1.1], METAL),
        ],
        ProductTable => {
            let mut v = vec![part("top", cube(1.0, 0.05, 0.7), [0.0, 0.725, 0.0], [0.92, 0.9, 0.86])];
            v.extend(legs4(["leg_1", "leg_2", "leg_3", "leg_4"], 0.9, 0.6, 0.7, 0.04, METAL));
            v
        }
        Softbox => {
            let mut v = light_stand(1.8);
            v.push(part("box", cube(0.62, 0.62, 0.3), [0.0, 1.8, 0.12], DARK));
            v.push(part("diffuser", cube(0.58, 0.58, 0.01), [0.0, 1.8, 0.28], WHITE));
            // the panel lights the scene: an area light facing the front (+Z)
            v.push(Part {
                rot: [0.0, 180.0, 0.0],
                light: Some(shotstage_core::LightData {
                    kind: shotstage_core::LightKind::Area,
                    size: [0.58, 0.58],
                    ..Default::default()
                }),
                ..part("light", cube(0.1, 0.1, 0.1), [0.0, 1.8, 0.3], WHITE)
            });
            v
        }
        Reflector => {
            let mut v = light_stand(1.2);
            v.push(Part {
                rot: [90.0, 0.0, 0.0],
                ..part(
                    "disc",
                    Primitive::Cylinder { radius: 0.4, height: 0.01, segments: 40 },
                    [0.0, 1.2, 0.05],
                    [0.85, 0.85, 0.82],
                )
            });
            v
        }
        LeafBranch => vec![
            Part { rot: [0.0, 0.0, 70.0], ..part("stem", cyl(0.004, 0.35), [0.0, 0.03, 0.0], [0.4, 0.3, 0.2]) },
            Part {
                rot: [0.0, 0.0, 20.0],
                scale: [0.09, 0.006, 0.04],
                modifiers: vec![array(4, [0.8, 0.0, 0.0])],
                ..part("leaves_1", ball(10), [-0.13, 0.05, 0.03], GREEN)
            },
            Part {
                rot: [0.0, 0.0, 20.0],
                scale: [0.08, 0.006, 0.035],
                modifiers: vec![array(3, [0.9, 0.0, 0.0])],
                ..part("leaves_2", ball(10), [-0.09, 0.06, -0.03], LIGHT_GREEN)
            },
        ],
        Flower => vec![
            part("stem", cyl(0.004, 0.28), [0.0, 0.14, 0.0], GREEN),
            Part { scale: [0.07, 0.03, 0.07], ..part("petals", ball(12), [0.0, 0.285, 0.0], [0.95, 0.6, 0.7]) },
            part(
                "centre",
                Primitive::Sphere { radius: 0.012, segments: 10, rings: 6 },
                [0.0, 0.3, 0.0],
                [0.95, 0.8, 0.2],
            ),
            Part {
                scale: [0.05, 0.004, 0.02],
                rot: [0.0, 30.0, 25.0],
                ..part("leaf", ball(8), [0.025, 0.12, 0.0], GREEN)
            },
        ],
        CitrusSlice => vec![
            Part {
                rot: [80.0, 0.0, 0.0],
                ..part(
                    "rind",
                    Primitive::Cylinder { radius: 0.035, height: 0.008, segments: 32 },
                    [0.0, 0.035, 0.0],
                    ORANGE,
                )
            },
            Part {
                rot: [80.0, 0.0, 0.0],
                ..part(
                    "flesh",
                    Primitive::Cylinder { radius: 0.031, height: 0.0085, segments: 32 },
                    [0.0, 0.035, 0.0],
                    [0.98, 0.75, 0.35],
                )
            },
        ],
        Pebbles => vec![
            Part { scale: [0.07, 0.035, 0.05], ..part("stone_1", ball(12), [0.0, 0.017, 0.0], [0.55, 0.55, 0.53]) },
            Part {
                scale: [0.05, 0.028, 0.04],
                rot: [0.0, 40.0, 0.0],
                ..part("stone_2", ball(12), [0.07, 0.014, 0.02], [0.4, 0.4, 0.42])
            },
            Part { scale: [0.04, 0.02, 0.03], ..part("stone_3", ball(12), [-0.05, 0.01, 0.04], [0.7, 0.68, 0.64]) },
            Part {
                scale: [0.06, 0.03, 0.045],
                rot: [0.0, -30.0, 0.0],
                ..part("stone_4", ball(12), [0.02, 0.015, -0.06], [0.48, 0.47, 0.45])
            },
        ],
        IceCubes => vec![
            Part { rot: [10.0, 20.0, 5.0], ..part("cube_1", cube(0.03, 0.03, 0.03), [0.0, 0.016, 0.0], GLASS) },
            Part { rot: [0.0, 55.0, 12.0], ..part("cube_2", cube(0.028, 0.028, 0.028), [0.045, 0.015, 0.02], GLASS) },
            Part { rot: [8.0, -25.0, 0.0], ..part("cube_3", cube(0.032, 0.032, 0.032), [-0.04, 0.017, 0.03], GLASS) },
            Part { rot: [-12.0, 10.0, 0.0], ..part("cube_4", cube(0.027, 0.027, 0.027), [0.01, 0.015, -0.045], GLASS) },
        ],
        Drape => vec![
            part("base", cube(1.0, 0.01, 0.6), [0.0, 0.005, 0.0], FABRIC_LIGHT),
            Part {
                rot: [0.0, 0.0, 90.0],
                scale: [1.0, 1.0, 0.55],
                ..part(
                    "fold_1",
                    Primitive::Capsule { radius: 0.05, height: 0.9, segments: 16 },
                    [0.0, 0.03, -0.12],
                    FABRIC_LIGHT,
                )
            },
            Part {
                rot: [0.0, 8.0, 90.0],
                scale: [1.0, 1.0, 0.5],
                ..part(
                    "fold_2",
                    Primitive::Capsule { radius: 0.04, height: 0.8, segments: 16 },
                    [0.05, 0.025, 0.1],
                    FABRIC_LIGHT,
                )
            },
            Part {
                rot: [0.0, -6.0, 90.0],
                scale: [1.0, 1.0, 0.5],
                ..part(
                    "fold_3",
                    Primitive::Capsule { radius: 0.035, height: 0.7, segments: 16 },
                    [-0.05, 0.02, 0.22],
                    FABRIC_LIGHT,
                )
            },
        ],
        Ribbon => vec![
            Part {
                rot: [90.0, 0.0, 0.0],
                scale: [1.0, 1.0, 0.25],
                ..part(
                    "loop",
                    Primitive::Torus { major_radius: 0.12, minor_radius: 0.012, major_segments: 32, minor_segments: 6 },
                    [0.0, 0.003, 0.0],
                    RED,
                )
            },
            Part { rot: [0.0, 30.0, 0.0], ..part("tail", cube(0.2, 0.004, 0.025), [0.18, 0.002, 0.05], RED) },
        ],
        Splash => vec![
            Part {
                rot: [180.0, 0.0, 0.0],
                ..part("crown", Primitive::Cone { radius: 0.11, height: 0.08, segments: 24 }, [0.0, 0.04, 0.0], WATER)
            },
            part("column", Primitive::Capsule { radius: 0.015, height: 0.16, segments: 10 }, [0.0, 0.12, 0.0], WATER),
            Part {
                modifiers: vec![array(3, [0.05, 0.035, 0.02])],
                ..part("drops", Primitive::Sphere { radius: 0.01, segments: 8, rings: 5 }, [-0.07, 0.12, -0.02], WATER)
            },
        ],
        DecorSpheres => vec![
            part("large", Primitive::Sphere { radius: 0.08, segments: 24, rings: 12 }, [0.0, 0.08, 0.0], PASTEL),
            part(
                "medium",
                Primitive::Sphere { radius: 0.05, segments: 20, rings: 10 },
                [0.13, 0.05, 0.03],
                [0.9, 0.75, 0.7],
            ),
            part("small", Primitive::Sphere { radius: 0.03, segments: 16, rings: 8 }, [0.07, 0.03, 0.1], GOLD),
        ],
    }
}

/// Tripod light stand with a pole up to `h`.
fn light_stand(h: f32) -> Vec<Part> {
    let mut v = vec![part("pole", cyl(0.015, h), [0.0, h / 2.0, 0.0], METAL)];
    for (n, yaw) in [("leg_1", 0.0f32), ("leg_2", 120.0), ("leg_3", 240.0)] {
        let (s, c) = yaw.to_radians().sin_cos();
        v.push(Part { rot: [0.0, yaw, 60.0], ..part(n, cyl(0.01, 0.5), [c * 0.2, 0.13, -s * 0.2], METAL) });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_prefab_builds() {
        for p in PREFABS {
            let parts = parts(p.kind);
            assert!(!parts.is_empty(), "{}", p.key);
            for part in &parts {
                if let Some(prim) = &part.prim {
                    let mut m = prim.build().unwrap_or_else(|e| panic!("{} {}: {e}", p.key, part.name));
                    for md in &part.modifiers {
                        m = md.apply(&m).unwrap();
                    }
                }
                if let Some(parent) = part.parent {
                    let i = parts.iter().position(|q| q.name == parent).expect("parent exists");
                    let me = parts.iter().position(|q| q.name == part.name).unwrap();
                    assert!(i < me, "{}: parent {parent} must come before {}", p.key, part.name);
                }
            }
            let mut names: Vec<_> = parts.iter().map(|q| q.name).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), parts.len(), "{}: duplicate part names", p.key);
        }
    }
}
