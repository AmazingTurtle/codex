fn main() {
    // sqlx's stable migrate! expansion tracks existing SQL files, but adding a
    // migration must also invalidate an already compiled embedding crate.
    println!("cargo:rerun-if-changed=build.rs");
    for directory in [
        "migrations",
        "logs_migrations",
        "goals_migrations",
        "memory_migrations",
        "queue_migrations",
        "thread_history_migrations",
    ] {
        println!("cargo:rerun-if-changed={directory}");
    }
}
