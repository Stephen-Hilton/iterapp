use chrono::Local;

fn main() {
    // Version = <major>.<minor>.<local build datetime>, e.g. 0.1.20260820.1434:
    // the datetime IS the patch level, so every rebuild self-increments and a
    // deployed binary can always be dated at a glance. Deliberately NO rerun-if
    // directives: cargo then reruns this script whenever any package file
    // changes, so the stamp moves exactly when the code actually rebuilt.
    println!(
        "cargo:rustc-env=ITER_VERSION={}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        Local::now().format("%Y%m%d.%H%M")
    );
}
