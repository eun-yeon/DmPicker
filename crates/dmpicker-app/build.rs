// exe 아이콘 (탐색기·작업 관리자에 보임), 트레이 아이콘은 assets/tray
fn main() {
    println!("cargo:rerun-if-changed=assets/dmpicker.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/dmpicker.rc", embed_resource::NONE).manifest_optional().unwrap();
}
