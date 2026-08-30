fn main() {
    // The frontend is embedded into the binary at compile time by
    // generate_context!. Without these, cargo has no idea that editing
    // src/*.css|html|js should invalidate the crate, so a rebuild after a
    // pure-frontend change is a silent cache hit that ships the OLD UI —
    // which looks exactly like "my fix did nothing".
    println!("cargo:rerun-if-changed=../src");
    for f in ["index.html", "styles.css", "app.js"] {
        println!("cargo:rerun-if-changed=../src/{f}");
    }
    tauri_build::build()
}
