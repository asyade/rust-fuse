
fn main () {
    if !cfg!(target_os = "android") {
        println!("cargo:rustc-link-lib=fuse");
    }
}