//! Compile the stylesheet and icons into a GResource bundle that `lib.rs`
//! embeds with `gio::resources_register_include!`.
fn main() {
    glib_build_tools::compile_resources(
        // The installed icon theme's artwork lives in the workspace's data
        // folder; the bundle reuses it rather than keeping a second copy.
        &["resources", "../../data/icons/hicolor"],
        "resources/resources.gresource.xml",
        "forti-vpn.gresource",
    );
}
