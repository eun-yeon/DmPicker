// exe 아이콘과 버전 정보 (탐색기·작업 관리자에 보임), 트레이 아이콘은 assets/tray
fn main() {
    println!("cargo:rerun-if-changed=assets/dmpicker.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let version = format!("{}.0", env!("CARGO_PKG_VERSION"));
    let macros = [
        format!("DMPICKER_VERSION={}", version.replace('.', ",")),
        format!("DMPICKER_VERSION_STRING=\"{version}\""),
    ];
    embed_resource::compile("assets/dmpicker.rc", macros).manifest_optional().unwrap();
}
