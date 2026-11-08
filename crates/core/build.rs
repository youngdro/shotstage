//! Fingerprints the built-in human asset, so a browser build (which downloads `human.bin`
//! instead of embedding it) can check that the file it got is the one it was built with.

fn main() {
    let path = "assets/human.bin";
    println!("cargo:rerun-if-changed={path}");
    let bytes = std::fs::read(path).expect("crates/core/assets/human.bin");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    println!("cargo:rustc-env=SHOTSTAGE_HUMAN_LEN={}", bytes.len());
    println!("cargo:rustc-env=SHOTSTAGE_HUMAN_HASH={h:016x}");
}
