//! Points the `embed-pdfium` feature at the PDFium library to compile in:
//! $PAGELENS_PDFIUM_LIB if set, else vendor/pdfium/lib/<platform name>.
fn main() {
    println!("cargo:rerun-if-env-changed=PAGELENS_PDFIUM_LIB");
    if std::env::var_os("CARGO_FEATURE_EMBED_PDFIUM").is_none() {
        return;
    }
    let path = std::env::var("PAGELENS_PDFIUM_LIB").unwrap_or_else(|_| {
        let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        let name = match target.as_str() {
            "macos" => "libpdfium.dylib",
            "windows" => "pdfium.dll",
            _ => "libpdfium.so",
        };
        let dir = if target == "windows" { "bin" } else { "lib" };
        format!("{}/vendor/pdfium/{dir}/{name}", std::env::var("CARGO_MANIFEST_DIR").unwrap())
    });
    if !std::path::Path::new(&path).exists() {
        panic!("embed-pdfium: PDFium library not found at {path} (set PAGELENS_PDFIUM_LIB or disable the feature)");
    }
    println!("cargo:rerun-if-changed={path}");
    println!("cargo:rustc-env=PAGELENS_PDFIUM_LIB={path}");
}
